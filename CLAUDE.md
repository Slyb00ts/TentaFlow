# CLAUDE.md

Guidance for Claude Code (claude.ai/code) working in this repo.

## Build & Run

No workspace Cargo.toml — each crate builds independently. Main binary: `tentaflow`.

```bash
cd tentaflow && cargo build                                   # main binary
cd tentaflow-core && cargo build --features dashboard-api     # core lib + dashboard

# Browser protocol glue (tentaflow-protocol-wasm). Without these two, build.rs
# skips www/js/protocol/wasm_glue.{js,wasm} and the dashboard won't load.
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.120 --locked     # MUST match the pinned crate

rustup target add wasm32-wasip1                               # WASM addons

./scripts/setup.sh                                            # one-shot (Linux + macOS)
```

Run: `./tentaflow/target/release/tentaflow --config <your.toml>` (config is user-provided).

`tentaflow-core` features — default = `inference-whisper` + `camera`. Key opt-ins:

| Flag | Purpose |
|------|---------|
| `dashboard-api` | Axum HTTP dashboard + API (opt-in; headless deploys skip it) |
| `docker` | Docker management (bollard) |
| `inference-llamacpp` / `inference-whisper` / `inference-sherpa` | llama.cpp / Whisper / sherpa-onnx backends |
| `inference-mlx*` | Apple MLX (macOS/iOS) |
| `inference-diarization` | speaker diarization (tentaflow-voice) |
| `gpu-cuda` | CUDA accel for llama.cpp + whisper |
| `test-support` | exposes `flow_engine::node_adapter::test_support` (for benches) |

`camera` is a default feature (GStreamer mandatory for the video pipeline).

## Configuration

`--config <toml>`. Sections: `[server]`, `[server.mtls]`, `[protocols.quic]`, `[mesh]`,
`[load_balancing]`, `[monitoring]`. Default HTTPS/QUIC port **8090**.

`[server.mtls]` (optional) — Service-to-Core mTLS pinning for `/core/frame/pickup`.
Production: `pickup_required = true` + at least one `client_cert_fingerprints` (SHA-256 hex).

## Transport architecture (2-tier) — every change must respect this split

### Tier 1: Binary primary (default)
WebTransport `/wt/api` + WebSocket `/ws/api` fallback, binary `MessageBody` (CBOR) protocol.
- **Frontend ↔ Core**: ALL admin UI, data fetching, chat, audio. The dashboard NEVER uses REST.
- **Addons ↔ Core** (wasmtime): host-function ABI via addon-sdk wrappers.
- **Services in mesh ↔ Core**: QUIC tunnel (mesh control plane).

### Tier 2: HTTP REST secondary — external integrations ONLY
- `POST /v1/*` (OpenAI-compatible: chat/completions, audio, embeddings) — for EXTERNAL
  apps using TentaFlow models. Auth: API key (`Authorization: Bearer`). NOT for the dashboard.
- `POST /core/frame/pickup` — Service-to-Core (yolo/whisper inference). HMAC `X-Pickup-Token`
  (one-shot, 30 s TTL). Production REQUIRES mTLS client-cert pinning.
- `GET /recordings/<ref>` / `GET /frames/<ref>` — signed-URL downloads (HMAC, TTL-bounded).

Security (both tiers): HMAC SHA-256 (constant-time via `subtle`), audit log per outcome,
per-IP + global rate limit (429 + `Retry-After`), path-traversal containment, security
headers (HSTS unconditional). Production TLS 1.3 only, AEAD ciphers only.

NEVER enable `RUST_LOG=hyper=debug` in production without a query-string scrubber — HMAC
tokens in `/recordings/<ref>?token=...` URLs would leak to logs via the request line.

## Flow engine

`tentaflow-core/src/flow_engine/`. DAG of typed nodes; `FlowEnvelope` carries a `FlowValue`
payload (Text/Json/Audio/Image/Video/Embedding/Other) + named artifacts. Entry points:
`execute_blocking` (full DAG) and `execute_streaming` (LLM streaming). Node adapters under
`node_adapters/`: trigger, llm, stt, tts, tts_clean, tts_stream_bridge, sentence_buffer,
combine, condition, pii_filter, memory, embeddings, conversation_history, output, plus
dynamic `addon.*` blocks. User-defined flows are validated (R1–R8) on save in
`dispatch/handlers.rs`. `FlowDispatcher` resolves a flow per `{model}:{service_type}:{modality}`
or falls back to a synthetic flow (`synthetic.rs`).

## Sync (decentralized data)

Target design in `docs/SYNC_LEDGER_PLAN.md`. SQLite = current addon/platform state;
embedded **Fjall** = Sync Ledger (operation log, outbox/inbox, ACK, cursors, hash-chain,
snapshots, compaction). The old CRDT mechanism is removed. The only active sync path is
`sync/ledger` + `sync/runtime`. Wire is binary CBOR over UFP/2 `channel=0x06 SyncLedger`
(not JSON). Platform tables synced per `sync/core_registry.rs`; runtime tables
(`flow_executions`, `audit_log`) are not. External secrets sync only via the allowlist
(`hf_token`, `ngc_api_key`), re-encrypted per node. Permission Engine + Sync Policy gate
which nodes receive which resources.

## Mesh

First-contact pairing over iroh ALPN `tentaflow-pairing/v2` (len-prefixed CBOR); handler
verifies `sender_node_id == iroh remote_id` and the Ed25519 key. Trust state lives in
`peer_persisted` / `peer_hints`. HMAC issuer keys mirror to trust-paired peers
(`MESH_MSG_HMAC_KEYS_SYNC`), in-memory only (revoke leaves no stale verifier). Cross-node
frame pickup proxies frame bytes over the mesh stream.

## Addons

`tentaflow-core/build.rs` embeds bundled addons from `tentaflow-core/addons/`
(addon.wasm + manifest.toml + `migrations/*.sql`). `bundle_hash` includes migrations, so a
schema change forces reconciliation. Host fn `http.request` is **fail-closed**: it needs an
admin-approved `[[network_rule]]` (addon Network tab) — manifest declarations are NOT
auto-approved, even with `required=true`.

Notable bundled addons: `eureka` (MF Eureka index → own SQLite), `company-lookup` (VAT
registry lookup, stateless), `contacts` (CRM source-of-truth: companies/persons/relations;
Flow blocks + app panels), `memory`, `embeddings-chunker`. Per-addon detail lives in each
addon directory.

## Admin Scheduler

`tentaflow-core/src/scheduler/`, state in SQLite (`scheduled_jobs`, `scheduled_runs`). Runs
addon tools via `AddonManager::call_tool`; the dashboard talks to it over the binary
protocol (`SchedulerBody`), not REST. Modes: `once` (RFC3339), `interval` (`30m`/`1h`/`1d`),
daily `cron` (`minute hour * * *`). UI in `www/js/modules/scheduler.js` (admin-only).

## Dashboard Settings

`Settings → Dostępy zewnętrzne`: external secrets (`hf_token`, `ngc_api_key`, container
registries) saved via the binary protocol with `is_secret`; listing returns `<redacted>` for
non-empty secrets. The vLLM recommender uses the stored `hf_token` to fetch `config.json`
from gated repos without persisting the token in the deployment config.

## Conventions

- Code comments, variable/function names, commit messages: **English**. Commit format:
  `[type]: description`.
- **NO AI / Claude attribution anywhere** — not in commits (`Co-Authored-By`), PR bodies, or code.
- Rust: rustfmt defaults, `snake_case` fns, `PascalCase` types. JS/HTML/CSS: 2-space indent,
  `camelCase` JS, `kebab-case` CSS. C#: 4-space, `PascalCase` public, `_camelCase` private.

## Code quality rules (MANDATORY — apply to every change)

Apply to humans AND every AI agent. No exceptions unless the user explicitly overrides a
specific rule for a specific task.

1. **No stubs, placeholders, or TODOs.** Every commit production-ready. Forbidden: `todo!()`,
   `unimplemented!()`, `// TODO: implement`, empty bodies returning defaults, mock responses,
   "wire this up later". A missing dependency → say so and stop; don't fake it.
2. **No backward-compat shims or fallbacks.** Change functions in place. No alias exports,
   deprecated wrappers, old-behavior feature flags, or `if let Some(old) ... else { new }` chains.
3. **No versioned function names** (`process_request_v2`, `do_thing_new`). Edit in place;
   update callers — that is the work. Git history is the version record.
4. **Check for existing functions before writing new ones.** Grep likely names/signatures/call
   sites. If one almost fits, extend it (new param / enum variant) rather than fork a parallel.
5. **Delete unused code as you go** — functions whose last caller you removed, unused imports,
   struct fields, CSS classes, i18n keys, SQL helpers. `cargo check` unused warnings are bugs.
6. **Comments describe WHY, not WHAT.** English only. File headers stay
   (`// ===== File: <name> — <purpose> =====`). Inline comments only for non-obvious intent
   (a workaround, an invariant, a perf trick). No `// CRITICAL:`, `// Fixed in this PR`,
   `// Changed from X to Y` — git blame carries history.
7. **Always use `tf-*` web components — never roll your own UI primitive.** 60+ components in
   `tentaflow-core/www/js/components/` (tf-button, tf-input, tf-select, tf-toggle, tf-checkbox,
   tf-radio, tf-textarea, tf-modal, tf-window, tf-table, tf-tabs, tf-menu, tf-searchbox,
   tf-combobox, tf-datepicker, tf-file-input, tf-toast, tf-tooltip, tf-chip, …). Use them for
   every primitive — zero raw `<button>`/`<input>`/`<select>`, hand-rolled modals, tab strips.
   Missing a feature → **extend the component**, don't fork. A pattern repeated in 2+ feature
   modules → add a new `tf-*` component. (There IS a `tf-file-input` — use it instead of raw
   `<input type="file">`.)

## gstack & skill routing

For web browsing use the `/browse` skill — never `mcp__claude-in-chrome__*`. Other gstack
skills (if installed): `/qa`, `/qa-only`, `/review`, `/ship`, `/investigate`, `/design-review`,
`/land-and-deploy`, `/canary`, `/benchmark`, `/codex`, `/cso`, `/careful`, `/freeze`, `/guard`.

When a request matches a skill, invoke it FIRST: bugs/errors → investigate; ship/deploy/PR →
ship; QA → qa; code review → review; architecture review → plan-eng-review; design polish →
design-review.
