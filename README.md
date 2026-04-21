# TentaFlow

**A distributed, self-hosted AI infrastructure platform.**

<!-- badges -->
![License](https://img.shields.io/badge/license-Apache%202.0-blue)
![Rust](https://img.shields.io/badge/rust-stable-orange)
![Platforms](https://img.shields.io/badge/platforms-Linux%20%7C%20macOS%20%7C%20Windows%20%7C%20Android%20%7C%20iOS-green)

## What is TentaFlow?

TentaFlow is a comprehensive, self-hosted AI platform designed to simplify running local models with various inference engines — vLLM, SGLang, MLX, llama.cpp, and others. The TentaFlow team provides ready-to-use Docker containers for different services, each with built-in QUIC communication to the TentaFlow engine. When you spin up a model with any supported engine, it automatically connects to the application and is ready to use — no manual wiring required.

Nodes discover each other via mDNS and form a decentralized mesh network. Requests are routed transparently across the mesh, so clients connect to any single node and get access to services running anywhere in the network.

TentaFlow includes a sandboxed WASM addon system where every function call and external communication is explicitly declared in the addon manifest. Administrators control exactly which permissions each addon receives, and every addon action is recorded in the Audit Log.

The TentaFlow team also trained a small **Qwen 3.5-0.8B** model that analyzes all incoming content and checks for hidden prompt injections and other security vulnerabilities before it reaches any service.

TentaFlow features a visual **Flow Builder** — a node-based DAG editor that lets you chain AI services, tools, and logic into multi-step workflows without writing code.

The entire application runs on mobile devices (Android and iOS) without requiring any server connection, providing the exact same capabilities as the server deployment — fully autonomous, fully offline.

## Features

### Mesh Networking

- Automatic node discovery via mDNS
- QUIC transport with mandatory TLS 1.3
- CRDT-based state synchronization across all nodes
- Multi-hop routing — nodes relay requests to reach services on non-adjacent peers
- Node pairing via 6-digit PIN
- SWIM-like failure detection (Ping / PingAck / IndirectPing)
- Service deduplication across the mesh
- Remote Docker management on peer nodes

Two ALPN protocols handle all communication:

| ALPN | Direction | Purpose |
|------|-----------|---------|
| `tentaflow` | SDK client &rarr; node | AI requests: chat, embeddings, TTS, STT, RAG, memory |
| `tentaflow-mesh` | node &harr; node | Heartbeat, CRDT sync, service discovery, forwarding, management |

### AI Services

- **LLM Inference** — llama.cpp (CPU/GPU), Apple MLX (Metal-accelerated), vLLM, SGLang, Ollama
- **Embeddings** — local embedding models served per-node
- **TTS / STT** — text-to-speech and speech-to-text pipelines
- **RAG** — document ingestion, chunking, retrieval
- **Memory** — fact extraction, summarization, contextual recall

### Dashboard & API

- Web SPA dashboard on port 8090 (vanilla JS, no framework dependencies)
- OpenAI-compatible API at `/v1/*`
- Dashboard API at `/api/*` (JWT-authenticated)
- WebSocket metrics streaming
- Prometheus metrics endpoint
- 15+ views: Dashboard, Services, Mesh, Clusters, Models, Playground, Flow Builder, Addons, Users, Audit Log, and more

### Addon System (WASM)

- Sandboxed execution via Wasmtime (wasmi on mobile)
- Permission-based access control
- Addon SDK for third-party developers
- Capabilities: event bus, key-value storage, outbound HTTP, UI injection
- Addon state synchronized across mesh via CRDT

### Orchestrator Model

A fine-tuned **Qwen 3.5-0.8B** model that acts as the routing brain:

- LoRA fine-tuned on 12 dataset types (guard, intent, tool selection, planning, memory, and more)
- Routes intents, selects tools and models, creates multi-step execution plans
- Special tokens: `<|guard|>`, `<|intent|>`, `<|tools|>`, `<|model|>`, `<|plan|>`, `<|memory|>`, `<|check|>`
- Exported as GGUF with multiple quantization levels (Q2 through Q8)

### Security

- TLS 1.3 mandatory on all connections (client-to-node and node-to-node)
- Ed25519 node identity keys + X25519 key exchange
- ChaCha20-Poly1305 AEAD encryption for management commands
- Argon2id password hashing
- JWT authentication (dashboard) + API key authentication (OpenAI endpoint)
- WASM sandbox isolation for all addons

## Architecture

### Crates

| Crate | Purpose |
|-------|---------|
| `tentaflow` | Main binary — API gateway and mesh node |
| `tentaflow-core` | Shared library — networking, routing, auth, mesh, addons, inference, API |
| `tentaflow-protocol` | QUIC protocol types with rkyv zero-copy serialization |
| `tentaflow-desktop` | Native desktop app (egui/wgpu) with system tray |
| `tentaflow-mobile` | Mobile runtime — Android (JNI) and iOS (Swift bridge) |
| `tentaflow-ui` | Shared UI framework (egui for native, HTML/CSS/JS SPA for web) |
| `tentaflow-client` | Client SDKs — native Rust (FFI) + .NET wrapper (P/Invoke over QUIC) |
| `tentaflow-models` | Training pipeline for the orchestrator model (LoRA, GGUF export) |
| `mlx-models` | Apple MLX inference bindings (Metal-accelerated, macOS/iOS) |

### Network Topology

```
┌──────────────────────────────────────────────────────────────┐
│                       MESH NETWORK                           │
│                                                              │
│   ┌─────────┐    tentaflow-mesh    ┌─────────┐               │
│   │ Node A  │◄════════════════════►│ Node B  │               │
│   │ (Linux) │    (QUIC, encrypted) │ (Linux) │               │
│   │ LLM x2  │                      │ STT     │               │
│   │ TTS     │                      │ RAG     │               │
│   └────┬────┘                      └────┬────┘               │
│        │                                │                    │
│        │ tentaflow-mesh                 │ tentaflow-mesh     │
│        │ (multi-hop)                    │                    │
│        │                           ┌────┴────┐               │
│        └──────────────────────────►│ Node C  │               │
│           A routes through B       │ (macOS) │               │
│                                    │ MLX     │               │
│                                    └─────────┘               │
│                                                              │
│   ┌─────────┐    tentaflow-mesh    ┌─────────┐               │
│   │ Node D  │◄════════════════════►│ Node E  │               │
│   │(Android)│                      │(Windows)│               │
│   │ Mobile  │                      │ GPU x4  │               │
│   └─────────┘                      └─────────┘               │
│                                                              │
│   ┌─────────────────────────┐                                │
│   │ Cluster "GPU-Farm"      │                                │
│   │  Node B + Node E        │                                │
│   └─────────────────────────┘                                │
└──────────────────────────────────────────────────────────────┘
         ▲                    ▲
         │ tentaflow          │ tentaflow
         │ (ALPN)             │ (ALPN)
    ┌────┴────┐          ┌────┴────┐
    │ SDK .NET│          │ SDK Rust│
    │ Client  │          │ Client  │
    └─────────┘          └─────────┘
```

## Platforms

| Platform | Runtime | Notes |
|----------|---------|-------|
| Linux | Native binary | Full feature set |
| macOS | Native binary | MLX inference support (Apple Silicon) |
| Windows | Native binary | Full feature set |
| Android | JNI bridge | Via `tentaflow-mobile` |
| iOS | Swift bridge | Via `tentaflow-mobile`, MLX on Apple Silicon |

## Getting Started

### Prerequisites

Install build dependencies before compiling from source.

**Ubuntu / Debian:**
```bash
sudo apt install build-essential pkg-config libssl-dev
```

**Fedora / RHEL:**
```bash
sudo dnf install gcc pkg-config openssl-devel
```

**Arch Linux:**
```bash
sudo pacman -S base-devel pkg-config openssl
```

**macOS:**
```bash
brew install openssl pkg-config
```

**Windows:**
```powershell
vcpkg install openssl:x64-windows
# or install via choco:
choco install openssl
```

WASM addons require the WASI target, and the browser protocol glue requires an additional target plus `wasm-bindgen` CLI:
```bash
rustup target add wasm32-wasip1           # for sandboxed addons
rustup target add wasm32-unknown-unknown  # for tentaflow-protocol-wasm
cargo install wasm-bindgen-cli --version 0.2.108 --locked
```

Without `wasm-bindgen`, `build.rs` skips generating `www/js/protocol/wasm_glue.{js,wasm}` and the dashboard fails to load in the browser (`codec.js` import error).

**One-shot install** (Linux + macOS): `./scripts/setup.sh` handles base dependencies, Rust toolchain, both WASM targets, and `wasm-bindgen-cli`.

**TLS certificates** are generated automatically during build (self-signed, EC P-256, valid 10 years) if `certs/cert.pem` and `certs/key.pem` are not present. No external tools required — generation uses pure Rust (`rcgen`). To use custom certificates, place them in `certs/cert.pem` and `certs/key.pem` before building.

### Building

```bash
cargo build --release
```

Feature flags on `tentaflow-core`:

| Flag | Description |
|------|-------------|
| `docker` | Docker container management |
| `inference-llamacpp` | llama.cpp inference backend |
| `inference-mlx` | Apple MLX inference backend |
| `dashboard-api` | Web dashboard and API |
| `metrics-prometheus` | Prometheus metrics endpoint |

### Running

```bash
./target/release/tentaflow --config config.toml
```

The dashboard is available at `https://localhost:8090`.

### Configuration

All settings live in `config.toml`:

```toml
# Node identity and role
[node]
name = "my-node"
role = "worker"          # "gateway" | "worker" | "hybrid"

# QUIC server
[server]
bind = "0.0.0.0:4433"

# Mesh networking
[mesh]
enabled = true
mdns = true
max_hops = 3

# Rate limiting, load balancing, monitoring...
```

## Client SDKs

### Rust (FFI)

C-compatible FFI library for embedding in any language. Communicates over QUIC with the `tentaflow` ALPN protocol.

### .NET

P/Invoke wrapper over the Rust FFI library with a high-level API:

```csharp
var client = new TentaFlowClient("node-address:4433");

var response = await client.ChatCompletionAsync(new ChatRequest
{
    Model = "llama3",
    Messages = [new("user", "Hello")]
});

var embeddings = await client.EmbeddingsAsync("Document text here");
```

Supported operations: ChatCompletion, Embeddings, RAG, TTS, STT, Ingest, Memory.

## License

Apache 2.0 — Copyright 2026 Slyb00ts. See [LICENSE](LICENSE) for details.
