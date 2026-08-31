<div align="center">

<img src="docs/logo.png" alt="TentaFlow" width="180" />

# TentaFlow

**An operating system for your AI.**

Turn every device you own - a GPU server, your laptop, your phone - into one private AI mesh.
Deploy models anywhere, wire them into flows, train your own models, and let TentaFlow pick the
right model automatically: the big one on the server when you're connected, the local one on your
phone when you're not.

![License](https://img.shields.io/badge/license-Apache%202.0-blue)
![Rust](https://img.shields.io/badge/rust-stable-orange)
![Platforms](https://img.shields.io/badge/platforms-Linux%20%7C%20macOS%20%7C%20Windows%20%7C%20Android%20%7C%20iOS-green)

</div>

---

## What is TentaFlow?

Most AI tools assume one machine, one model, and a cloud account. TentaFlow assumes the opposite:
you already have several devices with very different capabilities, and you want them to work together
as **one private AI system that you fully own**.

TentaFlow is the layer that makes that happen. It is a single Rust application that runs on Linux,
macOS, Windows, Android and iOS, and turns each device into a **node** in a peer-to-peer mesh.
A node can be a rack server with four GPUs, a MacBook, or a phone in your pocket - they all speak the
same protocol, share the same data, and expose the same capabilities.

On top of that mesh you:

- **Deploy models to any device** - run a 70B model on the GPU box and a small one on the phone, all from the same dashboard.
- **Build flows visually** — chain LLMs, speech, vision, documents, RAG, memory and tools into multi-step pipelines with the **Flow Builder**, no code required.
- **Define aliases with automatic fallback** - point your app at `assistant`, and TentaFlow uses the powerful server model when it's reachable and silently falls back to a local laptop/phone model when it isn't.
- **Train and benchmark your own models** — fine-tune LLMs (LoRA/QLoRA/DoRA, SFT/DPO/distillation), run tabular AutoML, annotate datasets and benchmark every model in **ML Studio** and **Benchmark Studio**.
- **Run agents, meetings, cameras and robots** — an agent harness with tools and skills, a meeting bot that transcribes and summarizes calls, a camera analytics pipeline, and robot control with LiDAR/SLAM.
- **Extend everything with addons** - sandboxed plug-ins (with their own UI) that add tools, integrations and data sources, written against an SDK.

And because the whole thing also runs **fully offline on a phone**, you get the exact same product whether
you're online with a server farm or on a plane with nothing but your handset.

<p align="center"><img src="docs/screenshots/chat.png" alt="TentaFlow chat — voice conversation with a fallback-aware model alias" width="900" /></p>

---

## The core ideas

### 🐙 One mesh, many devices

Every device runs the same node. Nodes find each other automatically over **iroh** (QUIC with relay,
DHT and LAN discovery), so they connect across the same Wi-Fi *or* across the internet without manual
port-forwarding. First contact is a simple **6-digit PIN pairing** (or QR-code scan on mobile) with
Ed25519 key verification — once two nodes are paired they trust each other. You can even self-host the
relay (`iroh-relay` ships as a service container).

A request sent to any node can be served by a service running on *any other* node: the mesh routes it
transparently, including multi-hop relays for peers that aren't directly connected. Your phone can use the
LLM on your server as if it were local.

State (your flows, settings, identities, RBAC, addon data) is kept consistent across the mesh by an
embedded **Sync Ledger** - an append-only, hash-chained, signed operation log with per-node cursors,
outbox/inbox and snapshots. Sync is permission-gated: a node only receives the resources it's allowed to.

<p align="center"><img src="docs/screenshots/mesh.png" alt="Mesh view — paired nodes, live resource usage and pending pairings" width="900" /></p>

### 🧮 Clusters: group nodes into one serving unit

Beyond ad-hoc mesh routing, nodes can be grouped into named **clusters** with a load-balancing strategy
and failover policy for distributed model serving. The cluster wizard probes live bandwidth between
candidate nodes, and the mesh layer auto-configures **RDMA / RoCE** where the hardware supports it.
Cluster detail shows per-node gauges, a live probe matrix and the models shared across the cluster.

### 🚀 Deploy any model to any node

TentaFlow runs models locally through several inference backends, and connects to external engines as
managed services:

| Capability | Backends |
|------------|----------|
| **LLM (local)** | llama.cpp (CPU/GPU, continuous batching + speculative decoding — ngram & self-speculative MTP), Apple **MLX** (Metal) |
| **LLM (managed engines)** | **vLLM** (CUDA/ROCm/Metal/DGX Spark), **SGLang**, **Ollama**, **ds4** (DeepSeek V4), TensorRT-LLM, **NVIDIA NIM** containers, Qwen3-VL |
| **LLM (cloud APIs)** | Anthropic, OpenAI / Azure OpenAI / OpenAI-compatible, Gemini, DeepSeek, Groq, Mistral, Moonshot, Qwen, Together, OpenRouter |
| **Speech-to-text** | Whisper, MLX-Whisper (Apple), sherpa-onnx, NVIDIA Parakeet, Qwen-ASR, Soniox (cloud) |
| **Text-to-speech** | Kokoro (MLX/ONNX), Kyutai TTS, sherpa-onnx, Supertonic (31 languages), XTTS (voice cloning), VoxCPM, Apple AVSpeech, ElevenLabs (cloud) |
| **Embeddings & rerankers** | Jina v5 / Nemotron / IBM Granite embeddings; Jina, Qwen3 and Nemotron rerankers (incl. multimodal) — each in vLLM, MLX and GGUF variants |
| **Vision** | face (YOLOv8 / SCRFD), pose (MoveNet / YOLOv8-pose), emotion (HSEmotion), object detection (RF-DETR), depth (Depth Anything V2/V3, native Rust ~27 ms), OCR (PaddleOCR, Apple OCR, ONNX OCR, license plates), NVIDIA Nemotron document-AI (parse, page elements, tables, graphics) |
| **Image generation** | ComfyUI, stable-diffusion.cpp |
| **Speaker diarization** | pure-Rust VAD + speaker embeddings (`tentaflow-voice`), voice-profile enrollment |

Services deploy as **Docker containers, native bundles (Python venv / prebuilt binaries) or external
endpoints** — the 4-step wizard detects your hardware (CUDA / Metal / Vulkan / XPU / CPU),
searches HuggingFace Hub, estimates VRAM and lets you pick GPUs per deployment. GPU acceleration is
available for llama.cpp and Whisper via CUDA, Vulkan and Metal. A built-in **vector database**
(`tentaflow-zvec`, embedded on every platform) powers semantic search, RAG and long-term memory;
external **Milvus** is supported for hybrid dense+sparse retrieval.

<p align="center"><img src="docs/screenshots/service-catalog.png" alt="Service Catalog — deploy LLM, STT and TTS engines to any node, Docker or native" width="900" /></p>

### 🔀 Flows: compose AI like building blocks

The **Flow Builder** is a visual, node-based editor (a typed DAG) for turning models and tools into
real pipelines - transcribe -> summarize -> translate -> speak, or trigger -> retrieve from memory ->
LLM -> filter PII -> output. The node palette spans **60+ block types**:

- **Models**: `llm` · `vision_llm` · `stt` · `tts` · `embeddings` · `reranker` · `vision_classify` · `ocr`
- **Documents & RAG**: `document_parse` · `text_extract` · `office_extract` · `pdf_rasterize` ·
  `table_structure` · `chunk` / `embed_chunks` · `vector` · `graph_search` · `rag_graphrag` · `rag_multihop`
- **Agents**: `agent_block` · `agent_router` · `spawn` / `await_subagents` · `tool_exec` · `ask_user`
- **Control & context**: `trigger` · `interval` · `condition` · `loop` · `map` · `subflow` ·
  `conversation_history` · `compact_context` · `memory` · `pii_filter` · `sentence_buffer` · `combine` · `output`
- **Cameras**: `camera_alert` · `camera_verdict` — plus dynamic `addon.*` blocks contributed by addons.

Flows run in two modes: **blocking** (full DAG, nodes run concurrently as their inputs become ready)
and **streaming** (token-by-token for LLM chat). Every flow is validated on save, autosaved with
version history, and can be scheduled or triggered by events.

<p align="center"><img src="docs/screenshots/flow-builder.png" alt="Flow Builder - a typed DAG chaining trigger, memory, LLM and TTS nodes" width="900" /></p>

### 🎯 Aliases with automatic fallback

This is the feature that makes a multi-device mesh actually pleasant to use.

An **alias** is a stable name (e.g. `assistant`, `coder`, `transcriber`) that points at a *primary* model
plus an ordered list of *fallback* models. Your apps and flows only ever reference the alias:

```
alias "assistant"
  ├─ primary:   qwen-72b           (on the GPU server)
  └─ fallback:  phi-3-mini-local   (on this laptop / phone)
```

At request time TentaFlow resolves the alias against what's actually reachable right now. It prefers a
**locally deployed** model over a remote one, walks the fallback chain on transport failures, and only
surfaces an error if *every* candidate is unreachable. So when you're at your desk you get the big server
model; when you walk away and lose the connection, the same `assistant` keeps working on-device - no code
change, no reconfiguration. Every resolution is audited (which target was used, whether a fallback kicked in).

### 🤖 Agents, skills and prompts

TentaFlow ships an **agent harness**: agents are defined declaratively (model, tool allowlist, skills,
limits) and the agent loop itself runs as a Flow Builder flow — not hard-coded Rust. Tools come from
addons and core builtins, resolved against per-agent allowlists and the permission system; every run is
recorded and inspectable. A **skills registry** holds reusable instructions (addon-provided skills can
be forked into editable user copies), with an LLM-assisted curator that proposes merges and cleanups
for admin approval. A central **prompt registry** gives system prompts stable IDs so engines can reuse
KV-cache across requests. **MCP** (Model Context Protocol) client addons expose external MCP servers'
tools as agent tools. There is even a dedicated **orchestrator model** (a fine-tuned 0.8B "conductor",
trained in `tentaflow-models`) for fast routing, tool selection and plan validation.

### 🎓 ML Studio: train your own models

ML Studio takes you from raw data to a deployed model, using compute from anywhere in the mesh:

- **LLM fine-tuning** — QLoRA / LoRA / DoRA / full fine-tune with SFT, DPO or logit-level
  knowledge distillation; base-model presets or any HuggingFace repo; live loss curves;
  VRAM estimates per method; export to GGUF/MLX for immediate deployment.
- **Model distillation** — generate Q→A pairs or preference triples with a teacher model,
  then train a smaller student.
- **Tabular AutoML & anomaly detection** — classification/regression leaderboards (AutoGluon)
  and anomaly detection, with automatic column profiling on upload.
- **Image recognition** — dataset schema, data collection, **annotation with model-assisted
  pre-labeling** (COCO), classifier and RF-DETR detector training, ONNX export.
- **Vision/audio fine-tuning** — adapt image and audio models to your own data.

Training jobs run as per-job service containers (HF SFT trainer, AutoGluon, timm, RF-DETR), stream
progress live to the dashboard, and register versioned artifacts that can be distributed to other
nodes over the mesh.

### 📊 Benchmark Studio

llama-bench-style benchmarking for every model you can reach — local engines, mesh services and
external cloud APIs. Wizard-driven target/test selection, live streaming runs, and results with
throughput (tokens/s), TTFT, prefill/decode split, latency percentiles (p50/p99) and concurrency
sweeps — plus side-by-side comparison of runs.

### 📷 TentaVision: cameras & video analytics

Ingest RTSP / ONVIF / local cameras (GStreamer) — or use a paired **phone as a camera and sensor
node** — and run on-frame models: face, pose, emotion, object detection, depth, license plates.
A detection bus feeds live overlays in the dashboard, `camera_alert` / `camera_verdict` flow blocks
turn detections into automations (notify, record, run an LLM verdict), and recordings are served
through HMAC-signed, TTL-bounded URLs. The architecture scales up to full surveillance pipelines
(tracking, re-identification, event correlation).

### 🦿 Robots, LiDAR and SLAM

Robots are first-class mesh citizens. An addon declares a `[robot]` manifest block (kind, transport,
capabilities, **safety envelope** with velocity clamps and mandatory e-stop) and the **Robots** app
renders a capability-driven control surface automatically: live camera, **LiDAR 3D view** (WebGPU
voxel renderer), controls generated from the robot's advertised actions, telemetry and logs. Robot
commands are allowlisted and clamped at the mesh layer; the e-stop is always available.

The reference integration is the **Unitree Go2** quadruped over WebRTC. Under the hood,
`tentaflow-slam` implements a unified SLAM loop (ESKF, LiDAR odometry, loop closure, pose graph)
and the core maintains a **shared, persistent occupancy map** folded from every robot's world-frame
LiDAR — including phones, which the `phone` addon turns into sensor-robots (camera, depth/LiDAR,
IMU, GPS, barometer with ESKF fusion).

### 🗣️ Meetings

The **Meeting Bot** joins calls (MS Teams), transcribes them live with speaker diarization, and
produces AI summaries and **extracted action items** in a live two-column view. Each session runs in
its own per-meeting container with a VNC window into the bot, and transcripts persist in SQLite —
searchable and downloadable after the fact.

### 🧩 Addons: extend everything, in your language

Addons are **sandboxed WebAssembly plug-ins** (WASM/WASI, run via Wasmtime on desktop, wasmi on mobile).
They add tools, data sources, Flow blocks, agent skills and even **their own dashboard panels** - the UI
is described declaratively (a typed CBOR component tree) and rendered natively by the host on web,
iOS and desktop.

There is a real **SDK** with host capabilities exposed through clean wrappers:

- LLM generate / stream / embeddings · per-addon **SQLite** and key-value storage
- outbound **HTTP** (fail-closed: admin must approve each network rule) · **web research** (search + readable-page extraction)
- events, timers, encrypted secrets, **OAuth** flows, camera / LiDAR / robot / sensor access, vector & graph & memory stores, document parsing, model aliases, and a typed UI builder

All SDK types come from a single source-of-truth spec (`tentaflow-sdk-spec`) and the SDKs are **generated
for Rust, C# and Python** (`tentaflow-sdk-gen`) - so addons aren't locked to one language.

Bundled addons include: `memory` (knowledge graph + vector memory with REM-style consolidation),
`rag` (independent RAG instances: ingest, chunk, embed, search), `deep-research`, `contacts` + `crm` +
`company-lookup` (CRM stack with official registry lookups), `mcp` / `ibm-mcp` (MCP clients),
`go2` (Unitree robot), `phone` (mobile sensor node), `tentavision`, `embeddings-chunker`, `eureka`.

<p align="center"><img src="docs/screenshots/addons.png" alt="Add-ons — sandboxed WASM plug-ins with per-addon permissions" width="900" /></p>

### 📱 The same product, fully offline

The mobile build (Android via JNI, iOS via a Swift bridge) is **not a thin client** - it's the whole node:
local inference, the flow engine, addons, the sync ledger and the dashboard, all on-device. Pair it with
your other nodes to share their models, or run it standalone on a plane. Same capabilities either way.
The phone's camera, depth sensor, IMU and GPS can also feed the mesh as a roaming sensor node.

---

## More that's built in

- **Web dashboard** - a fast vanilla-JS SPA on port `8090` with **40+ views** built from ~80 shared
  `tf-*` web components, localized in **5 languages** (pl, en, fr, es, de). It never uses REST — it
  talks to the core over a binary CBOR protocol. Admins get the full console; regular users get a
  tiled **apps home** (Chat, Notes, Translate, Meetings, Robots…).
- **OpenAI-compatible API** — `POST /v1/chat/completions`, `/v1/audio/*`, `/v1/embeddings`,
  `/v1/images/generations`, `/v1/rerank` + `/v1/ranking`, `/v1/depth`, `/v1/infer`, plus an
  **Anthropic-compatible `/v1/messages`** endpoint — with API keys, per-model ACLs (denied models
  return 404) and interactive docs at `/docs`. External apps can use either **direct model
  passthrough** or a **flow-as-model** (any flow exposed under a model name).
- **Access control** - users, groups, roles catalog, and a tri-state **allow/deny/inherit permission
  matrix** over every resource (models, flows, addons, robots, cameras), default-deny, enforced
  server-side. API keys are scoped, rotatable and sync-aware.
- **Compliance core (GDPR/RODO)** - built-in AI audit, retention policies, ROPA, DSAR, consents, DPIA,
  breach register and generated legal documents, with every AI call linked into a tamper-evident audit chain.
- **Token accounting** - per-org quotas and limits, usage dashboards, model analytics with billing/pricing,
  and a distributed token-lease coordinator (rendezvous-hash elected) for mesh-wide budget enforcement.
- **Scheduler** - run addon tools and flows on a cron / interval / one-shot schedule with retry policies
  and run history.
- **Profiling suite** - multi-source, mesh-wide profiling sessions (CPU flamegraphs, per-vendor GPU,
  memory, disk, power; NVIDIA Nsight integration) with a unified timeline, reports and session comparison.
- **Web research for addons** - pluggable search providers (SearXNG, Brave, Tavily, DuckDuckGo) and a
  SSRF-guarded readable-page reader, optionally backed by a headless-Chromium renderer service.
- **Service containers** - ship engines as Docker images *or* native bundles (Python venv, prebuilt
  binaries), deployable to any node from the dashboard; includes infra services (SearXNG, browser
  renderer, Milvus, self-hosted iroh relay).

## Security

- **TLS 1.3 everywhere** (client↔node and node↔node), AEAD ciphers only in production.
- **Ed25519** node identities, key-verified pairing (PIN rate-limited), HMAC (constant-time) on REST
  integration endpoints, optional **mTLS client-cert pinning** for service callbacks.
- **WASM sandbox** isolation for addons; host functions are fail-closed and require admin-approved
  permissions and network rules; addon signature verification; a deliberately malicious test addon
  guards the sandbox in CI.
- **Argon2id** password hashing, JWT for the dashboard, scoped API keys for the `/v1` API,
  ACL precedence user-deny > user-allow > group-deny > group-allow > default.
- Per-IP + global rate limiting, tamper-evident audit chain, path-traversal containment,
  unconditional HSTS, SSRF guards on all outbound fetching, signed TTL-bounded download URLs.

---

## Architecture at a glance

```
                          ┌───────────────── MESH (iroh / QUIC, encrypted) ─────────────────┐
                          │                                                                  │
   ┌──────────────┐       │   ┌──────────────┐        ┌──────────────┐      ┌────────────┐  │
   │  GPU server  │◄──────┼──►│   Laptop     │◄──────►│    Phone     │◄────►│   Robot    │  │
   │  vLLM 72B    │       │   │  llama.cpp   │  multi │  MLX small   │      │  Go2 lidar │  │
   │  training    │       │   │  flows       │  hop   │  offline ok  │      │  camera    │  │
   └──────────────┘       │   └──────────────┘        └──────────────┘      └────────────┘  │
                          │      ▲  Sync Ledger (state) · clusters · alias resolution        │
                          └──────┼───────────────────────────────────────────────────────────┘
                                 │
              ┌──────────────────┼──────────────────────────────┐
              │ binary CBOR (dashboard/SDK)     REST /v1/* (external apps, OpenAI/Anthropic-compatible)
        ┌─────┴─────┐            ┌──────┴──────┐
        │ Dashboard │            │  Your app   │
        │   (SPA)   │            │ (any lang)  │
        └───────────┘            └─────────────┘
```

### Crates

| Crate | Purpose |
|-------|---------|
| `tentaflow` | Main binary — mesh node + API gateway |
| `tentaflow-core` | The engine — networking, mesh, sync, routing, auth, inference, flows, agents, ML Studio, benchmark, meetings, cameras, robots, addons, API, dashboard |
| `tentaflow-protocol` / `-wasm` | Wire protocol (CBOR) + browser WASM glue |
| `tentaflow-transport` | Shared iroh + CBOR transport layer |
| `tentaflow-wrappers` | Native engine wrappers — llama.cpp continuous-batching engine with speculative decoding (ngram / MTP), whisper.cpp |
| `tentaflow-desktop` | Native desktop app (egui/wgpu) with system tray — macOS, Windows, Linux |
| `tentaflow-mobile` | Mobile runtime — Android (JNI/Kotlin) + iOS (Swift bridge, MLX/Whisper/Kokoro engines) |
| `tentaflow-voice` | Pure-Rust VAD + speaker embeddings (diarization), no onnxruntime |
| `tentaflow-zvec` / `-sys` | Embedded vector database |
| `tentaflow-slam` | Unified SLAM core — ESKF, LiDAR odometry, loop closure, pose graph |
| `tentaflow-voxel-wasm` | Browser WebGPU/WebGL point-cloud renderer (LiDAR 3D view) |
| `tentaflow-hardware` | Native device/robot integrations (Unitree Go2 over WebRTC) |
| `tentaflow-containers` | Service container definitions — 80+ engines across LLM/STT/TTS/vision/embeddings/rerankers/training/infra |
| `tentaflow-sdk-spec` / `-gen` | Addon SDK type spec + Rust/C#/Python code generators |
| `tentaflow-ui` / `-ui-schema` | Shared UI framework + declarative addon-UI schema |
| `tentaflow-client` | Client SDKs — native Rust FFI + .NET wrapper |
| `tentaflow-cli` | CLI — addon manifest validation, packaging |
| `tentaflow-models` | Training pipeline for the orchestrator ("conductor") model |

---

## Getting started

### Prerequisites

**Ubuntu / Debian:** `sudo apt install build-essential pkg-config libssl-dev`
**Fedora / RHEL:** `sudo dnf install gcc pkg-config openssl-devel`
**Arch:** `sudo pacman -S base-devel pkg-config openssl`
**macOS:** `brew install openssl pkg-config`

The dashboard's browser protocol glue needs two WASM targets and a pinned `wasm-bindgen`:

```bash
rustup target add wasm32-wasip1            # sandboxed addons
rustup target add wasm32-unknown-unknown   # browser protocol glue
cargo install wasm-bindgen-cli --version 0.2.125 --locked   # MUST match the pinned crate
```

> Without `wasm-bindgen`, `build.rs` skips `www/js/protocol/wasm_glue.{js,wasm}` and the dashboard won't load.

**One-shot setup** (Linux + macOS) handles toolchain, both targets and `wasm-bindgen`:

```bash
./scripts/setup.sh
```

> On macOS 26+ (Xcode 26) the Metal compiler is a separate component. Without it, MLX models return
> gibberish with no build error — `setup.sh` installs it and `build.rs` fails loudly if it's missing.

TLS certs are generated automatically on first build (self-signed EC P-256, pure Rust via `rcgen`);
drop your own into `certs/cert.pem` + `certs/key.pem` to override.

### Build & run

```bash
cd tentaflow && cargo build --release --features gpu-cuda
./target/release/tentaflow --config ../config.toml
```

Open the dashboard at **https://localhost:8090**.

Useful `tentaflow-core` features: `inference-llamacpp`,
`inference-whisper` (default), `inference-sherpa`, `inference-mlx*` (Apple), `inference-diarization`,
`gpu-cuda`, `gpu-vulkan`, `docker`.

Warianty głównej ścieżki vision:

```bash
# NVIDIA: dotychczasowy ORT/TensorRT/CUDA
cargo build --release --features gpu-cuda

# AMD/Intel: Burn przez WGPU/Vulkan
cargo build --release --features gpu-vulkan
```

Karty AMD i Intel jadą na Vulkan/WGPU — zarówno główna ścieżka vision (RF-DETR, Stan
i OCR), jak i llama.cpp. Nie wymaga to CUDA ani `nvcc`. HIP/ROCm nie jest wspierany:
utrzymywanie dwóch wykluczających się backendów ggml (te same symbole) dawało build
zależny od tego, który sterownik odpowiedział w trakcie kompilacji.
CUDA-owy preprocessing zero-copy pozostaje osobną, jawną funkcją `gpu-cuda`/`vision-cuda`;
Supertonic może nadal korzystać z ORT, ale nie wymusza już ORT dla wizji. NVIDIA z
`gpu-cuda` zachowuje ścieżkę ORT/TensorRT/CUDA.

### Configuration

A single TOML file passed with `--config`. Main sections: `[server]`, `[server.mtls]`,
`[server.tls]`, `[protocols.quic]`, `[mesh]`, `[load_balancing]`, `[monitoring]`. Default
HTTPS/QUIC port **8090**.

The HTTPS certificate is generated per installation on first start into `<data>/tls/`
(EC P-256, SANs: `localhost`, the hostname, every local IP) and regenerated automatically
when the local addresses change. Add names clients will use to reach the node with:

```toml
[server.tls]
extra_sans = ["192.168.11.26", "tentaflow.lan"]
```

---

## License

Apache 2.0 — Copyright 2026 Slyb00ts. See [LICENSE](LICENSE).
