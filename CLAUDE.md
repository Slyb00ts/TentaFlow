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
