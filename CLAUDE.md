# CLAUDE.md

Guidance for Claude Code (claude.ai/code) working in this repo. This file holds **invariants,
traps and rules** — not change history. Measurements, plans and phase reports live in `docs/`
and `tentaflow-infer/docs/`.

## Build & Run

No workspace Cargo.toml — each crate builds independently. Main binary: `tentaflow`.

```bash
cd tentaflow && cargo build                                   # main binary
cd tentaflow-core && cargo build                              # core lib + dashboard

# Browser protocol glue (tentaflow-protocol-wasm). Without these two, build.rs
# skips www/js/protocol/wasm_glue.{js,wasm} and the dashboard won't load.
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.125 --locked     # MUST match the pinned crate

rustup target add wasm32-wasip1                               # WASM addons

./scripts/setup.sh                                            # one-shot (Linux + macOS)
```

Run: `./tentaflow/target/release/tentaflow --config <your.toml>` (config is user-provided).

**macOS trap.** macOS 26+ (Xcode 26) split the Metal compiler into a separate component.
Without it `xcodebuild` builds a broken `mlx.metallib` and EVERY MLX model returns gibberish
(wrong GPU logits) with no build error. `setup.sh` installs it; `tentaflow/build.rs` fails
loudly if missing. Fix: `xcodebuild -downloadComponent MetalToolchain` + drop the stale
metallib (`rm -rf tentaflow-desktop/macos/swift/MLXBridge/build-xcode`). MLXBridge builds via
`xcodebuild -skipMacroValidation` (SwiftPM CLI can't compile Metal shaders).

`tentaflow-core` features — default = `inference-whisper` + `camera` (GStreamer is mandatory
for the video pipeline). Key opt-ins:

| Flag | Purpose |
|------|---------|
| `docker` | Docker management (bollard) |
| `inference-llamacpp` / `inference-whisper` / `inference-sherpa` | llama.cpp / Whisper / sherpa-onnx backends |
| `inference-mlx*` | Apple MLX (macOS/iOS) |
| `inference-diarization` | speaker diarization (tentaflow-voice) |
| `gpu-cuda` | CUDA accel for llama.cpp + whisper |
| `gpu-vulkan` | Vulkan for llama.cpp + portable WGPU vision backend |
| `gpu-rocm` / `vision-rocm` | HIP/ROCm for llama.cpp + optional Burn vision backend |
| `vision-cuda` | Burn vision CUDA backend + zero-copy CUDA preprocessing |
| `vision-ort` | ORT/TensorRT/CUDA for the main vision path; pulled in by `gpu-cuda` |
| `test-support` | exposes `flow_engine::node_adapter::test_support` (for benches) |

## Configuration

`--config <toml>`. Sections: `[server]`, `[server.mtls]`, `[server.tls]`, `[protocols.quic]`,
`[mesh]`, `[load_balancing]`, `[monitoring]`. Default HTTPS/QUIC port **8090**.

`[server.mtls]` (optional) — Service-to-Core mTLS pinning for `/core/frame/pickup`.
Production: `pickup_required = true` + at least one `client_cert_fingerprints` (SHA-256 hex).

`[server.tls]` (optional) — `extra_sans = [...]` added to the per-installation HTTPS
certificate (`api/tls_identity.rs`): generated with rcgen into `<data>/tls/{cert,key}.pem`
(EC P-256, CN = hostname, SANs = localhost + hostname + every local IP + extras), regenerated
when a desired SAN is missing from the stored cert, `certs/cert.pem` embedded only as the
fallback when `<data>/tls` is unusable. `/v1/models` reports `supports_embeddings` and
`supports_structured_output` per entry (the latter is true only for a LOCAL HTTP-transport
chat service — embedded, QUIC, mesh-forwarded and flow paths drop `response_format`).
`ChatCompletionRequest.extra` / `ResponseFormat.{json_schema,extra}` pass unknown vendor
fields through to HTTP backends verbatim. `/v1/embeddings` accepts `input` as a string or array
of strings only (token-id arrays → 400 `invalid_request_error`, no backend is token-in),
forwards `dimensions`/`user`/`EmbeddingRequest.extra` to HTTP backends (via
`envelope.meta["embeddings_user"|"embeddings_extra"]` on the flow path), always talks base64
to the backend and re-encodes at the edge per the client's `encoding_format`
(`EmbeddingResponse::to_wire_json`); `response.model` echoes the requested id.

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
- `GET /models/manifest/<bundle>` / `GET /models/file/<bundle>/<name>` — vision model-bundle
  sharing between instances. Auth: signed URL OR `Authorization: Bearer <api-key>` with an
  explicit `('model_bundle', <bundle_ref>)` allow scope (default-DENY, general keys only) —
  works between UNPAIRED instances.
- `GET /project-studio/exports/<ref>` — Project Studio archive, signed-URL scope
  `ProjectStudioExport`.

Security (both tiers): HMAC SHA-256 (constant-time via `subtle`), audit log per outcome,
per-IP + global rate limit (429 + `Retry-After`), path-traversal containment, security
headers (HSTS unconditional). Production TLS 1.3 only, AEAD ciphers only.

NEVER enable `RUST_LOG=hyper=debug` in production without a query-string scrubber — HMAC
tokens in `/recordings/<ref>?token=...` URLs would leak to logs via the request line.

## Flow engine

`tentaflow-core/src/flow_engine/`. DAG of typed nodes; `FlowEnvelope` carries a `FlowValue`
payload (Text/Json/Audio/Image/Video/Embedding/Other) + named artifacts. Entry points:
`execute_blocking` (full DAG) and `execute_streaming` (LLM streaming). Node adapters under
`node_adapters/`: trigger, llm, stt, tts (blocking + streaming, one node), tts_clean,
sentence_buffer, combine, condition, pii_filter, memory, embeddings, conversation_history,
project_knowledge, output, plus dynamic `addon.*` blocks. User-defined flows are validated
(R1–R8) on save in `dispatch/handlers.rs`. `FlowDispatcher` resolves a flow per
`{model}:{service_type}:{modality}` or falls back to a synthetic flow (`synthetic.rs`).

Embedded streaming + tools does not work — a streaming LLM node runs WITHOUT tools.

## Sync (decentralized data)

Design: `docs/SYNC_LEDGER_PLAN.md`. SQLite = current addon/platform state; embedded **Fjall**
= Sync Ledger (operation log, outbox/inbox, ACK, cursors, hash-chain, snapshots, compaction).
The only active sync path is `sync/ledger` + `sync/runtime`; wire is binary CBOR over UFP/2
`channel=0x06 SyncLedger`. Platform tables synced per `sync/core_registry.rs`; runtime tables
(`flow_executions`, `audit_log`) are not. External secrets sync only via the allowlist
(`hf_token`, `ngc_api_key`), re-encrypted per node. Permission Engine + Sync Policy gate which
nodes receive which resources. Startup runs `ensure_default_core_sync_policies` +
`ensure_trusted_nodes_in_sync_identity` so Flow Builder, shared settings, identity and RBAC
reach the outbox without hand-seeding policies.

## Mesh

First-contact pairing over iroh ALPN `tentaflow-pairing/v2` (len-prefixed CBOR); the handler
verifies `sender_node_id == iroh remote_id` and the Ed25519 key. Trust state lives in
`peer_persisted` / `peer_hints`. HMAC issuer keys mirror to trust-paired peers
(`MESH_MSG_HMAC_KEYS_SYNC`), in-memory only (revoke leaves no stale verifier). Cross-node
frame pickup proxies frame bytes over the mesh stream.

## Meeting Bot

Core is the iroh CLIENT of the per-meeting `teams-bot` sidecar. `MeetingManager::start_session`
registers a `QuicServiceHandle` (`iroh://<bot_endpoint_id>` + direct addr `127.0.0.1:<quic_port>`)
under `meeting-bot-<session_id>` via `ServiceManager::register_meeting_bot`; the reconnect loop
(`handles_cache::spawn_quic_reconnect_loop`) attaches `services/runtime/reverse_listener.rs`
(`accept_bi` on the connection Core dialled) ONLY when the handle carries a `ReverseWiring` —
set only for engines whose manifest declares `reverse_requests = true` (teams-bot does). Any other
service's `open_bi` is never accepted; per-service concurrency is capped
(`DEFAULT_MAX_REVERSE_STREAMS`), frames by `tentaflow_transport::MAX_FRAME_SIZE`.

The bot never picks models or flows: every speech segment is one `ModelPayload::FlowInvoke`
(`stream=true`) served by `meeting/flow_turn.rs` — session looked up by `meeting_id`, the
`container_name` must equal the caller's service name (a bot cannot drive another meeting),
pipeline (`stt_alias`/`llm_alias`/`tts_alias`/`flow_id`, migration 131, default flow
`MEETING_BOT_FLOW_ID`) read from the session row. Raw PCM is wrapped into WAV for STT, the
meeting context (active speaker, roster, last transcripts) is the trigger's `input_0`, meta
follows the envelope contract (`output_audio`, `model`, `stt_model`, `tts_model`, `format=pcm`).
Chunk order: `Transcript` (recorded + diarized BEFORE the first token), `TextDelta`, `AudioChunk`,
`Done`. The first 32 bytes of text are held back for the `<NO_RESPONSE>` marker — on a hit the
execution is cancelled and nothing (no held audio) reaches the meeting. Only
`SUMMARIZATION_ALIAS` still travels to the bot as env; `RESPOND_ENABLED=false` makes the bot send
`respond=false` and Core returns the transcript only.

A sidecar is NOT a trusted peer. Both reverse entry points take `caller: Option<&ReverseCaller>` —
`None` is mesh forwarding (trust-paired node, unchanged access), `Some` is a container. For a
container the streaming path accepts ONLY `FlowInvoke`, and the unary path accepts only `Audio`,
`Completion`, `PromptFetch` and `MeetingEvent`, each bound to a meeting the caller owns through
the one gate `meeting/flow_turn.rs::lookup_owned_session` (`container_name == service_name`, not
ended/leaving). Everything else (`Embeddings`, `Vision`, `Rerank`, `CameraCv`, `Documents`, …) is
`Unauthorized` — Core is not an anonymous inference proxy. `Completion`/`PromptFetch` stay open
because the bot's summarizer uses them; that is why the bot puts `meeting_id` into
`ModelRequest.metadata` for both.

A broken reverse stream must stop the work: `run_flow_turn` arms a `DropGuard` on the request's
`CancellationToken` (disarmed only when the stream ended by itself), so aborting the producer task
cancels LLM + TTS instead of running to the 120 s turn deadline. Independently,
`finalize_streaming_flow` cancels when `outbound_tx.send` fails — a dropped consumer never leaves a
flow grinding. Transcripts and answers are personal data: logs on these paths carry lengths,
latencies and ids only, never the text.

## Addons

`tentaflow-core/build.rs` embeds bundled addons from `tentaflow-core/addons/` (addon.wasm +
manifest.toml + `migrations/*.sql`). `bundle_hash` includes migrations, so a schema change
forces reconciliation (a rebuilt bundled addon stays down until the new hash is approved).

Host fn `http.request` is **fail-closed**: it needs an admin-approved `[[network_rule]]`
(addon Network tab) — manifest declarations are NOT auto-approved, even with `required=true`.
`[[network_rule]].host` supports exact hosts, `*.domain` wildcards and `*`. Wildcards keep the
declared port and stay behind the host SSRF guard (public destinations only); exact-host rules
may target ANY address including private/LAN/loopback — that is the intended way to reach a
LAN service. Exact beats wildcard. `http.request` never follows redirects (the addon receives
the raw 3xx).

Host fns `directory_users/groups/roles/org_v1` (scope `directory.read`) expose the org identity
catalog. Rust addons get typed UI catalog v1 bindings via `scripts/gen-rust.sh` →
`addon-sdk/sdk/src/ui_v1/` (same codegen pipeline as C#/Python).

Notable bundled addons: `eureka`, `company-lookup`, `contacts` (CRM source of truth), `memory`,
`embeddings-chunker`, `notes` (SQLite source of truth + `graph_outbox` → Cozo `notes_kg` + zvec
namespaces, hybrid RRF search), `deep-research` (thin facade over the web-research service).
Protocol `SCHEMA_VERSION` 23 — old/new binaries reject each other on handshake; rebuild all
mesh nodes together.

### Web research

`tentaflow-core/src/web_research/` is the central public-web service for addons (scope
`web.research`, SDK `web_*` wrappers): search providers (`searxng`, `duckduckgo`, `brave`,
`tavily`), public URL reading with DNS pinning / redirect revalidation / body limits, and batch
`read_search_results` (`search_limit` = candidate pool, keep reading until `read_limit` pages
succeed).

Provider resolution when the request omits `provider`: local running `searxng` service →
otherwise a visible remote `searxng` in the mesh registry via `MeshCommandType::WebResearch`
(so mobile nodes need no direct HTTP to another node's loopback) → otherwise public DuckDuckGo
HTML. Page URLs from search always go through the public-URL SSRF guard.

Reading is not a tag-stripper: `reader.rs` fetches only public readable content types, then
`extract.rs` runs `readability` (no independent HTTP client), falling back to local semantic
block scoring. Responses carry extraction method, char/word counts and `quality_score`.
`mode="auto"` uses Browser Renderer when one is reachable, `mode="browser"` requires it,
`mode="static"` forces the HTTP/readability path.

## Service Containers

`tentaflow-containers/tools/_services/*.toml` register infra services with a Docker and a
native (`python-bundle`) deployment.

- `searxng.toml` — public-web search; both variants enable JSON output for `web_research`.
  Not available natively on iOS (it is a Python web app, not an embedded Rust engine).
- `browser-renderer.toml` — Playwright Chromium for JS-heavy pages; exposes `/render`,
  `/contexts`, `/health`, one persistent context per `user_id` (isolated cookies/storage),
  idle eviction by `BROWSER_RENDERER_CONTEXT_TTL_SECONDS`, cap
  `BROWSER_RENDERER_MAX_CONTEXTS`. It validates public URLs before navigation and aborts
  private/local subresource requests inside Playwright routing.
- `test-runner` — see Project Studio below.

LLM engine versions live in the container manifests: vLLM 0.27.1 on PyTorch 2.13.0 +
torchvision 0.28.0, SGLang 0.5.17 on PyTorch 2.11.0; ROCm vLLM variants install from the
official ROCm wheel index. DGX Spark builds vLLM 0.27.1 from source and the DeepSeek DSpark
variant applies only the local `nvfp4_ds_mla` patch (DSpark support landed upstream). Profile
`vllm-dspark` is legacy (external vLLM 0.24 image + old overlay) — 0.27.1 needs
`vllm-dspark-src`, because the old overlay no longer applies to current sources.

vLLM recipes (`tentaflow-core/src/deploy/vllm_recipes.rs`) resolve in a cascade: exact HF id →
normalized id (quantization/packaging suffixes stripped, same org then canonical orgs) →
architecture family from `config.json` (`Qwen3_5*`, `Glm4*`, `DeepseekV3/V4*`, `Llama*`, …).
`vllm-recipes/recipes.json.gz` is regenerated by `scripts/update-vllm-recipes.sh`; hand-curated
entries go in `vllm-recipes/supplement.json` (upstream wins on overlap). The VRAM calculator counts
KV only for `full_attention` layers (`layer_types` / `full_attention_interval`) and adds the GDN
recurrent state for `linear_attention` layers.

Multi-GPU NCCL engines (vLLM/SGLang, native + docker) get `NCCL_P2P_LEVEL=SYS` automatically
(`services/deploy/gpu_topology.rs`, `nvidia-smi topo` cached per process) ONLY when a selected pair
is NODE/SYS with P2P OK and no NVLink; any `engine_env.NCCL_P2P_LEVEL` (even empty) disables it.

## ML Studio training tracks

`tentaflow-core/src/ml_studio/` drives three vision tracks off ONE COCO dataset, each with its
own Python training service under `tentaflow-containers/training/`:

| Track | Core driver | Service | Artifact |
|-------|-------------|---------|----------|
| detection (17 ADR classes) | `train_recognition.rs` | `rfdetr-training` :8202 | RF-DETR checkpoint |
| attribute classifier (`stan`) | `train_classifier.rs` | `classifier-training` :8203 | ONNX timm |
| OCR reader (`kod`) | `train_ocr.rs` | `ocr-training` :8204 | `adr_ocr.onnx` + alphabet |

**The `approved` gate is mandatory for every track.** Only images with `approved: true` enter
training; unreviewed auto-label predictions (`predicted: true`) must never train the model on
its own output. Detection filters in `pool_splits` (Core), the other tracks in `_collect_*` of
the service. A dataset where NO image has an `approved` field never passed our editor (external
COCO) and is used whole.

OCR trains on plate ROWS: an `ocr` attribute value `<kemler>/<UN>` (e.g. `99/3257`) labels the
top and bottom row, cut with the same 6% gap as runtime (`SPLIT_MARGIN` in `vision/adr_ocr.rs`).
Real rows are mixed with synthetics generated from the deployment's `adr-list.json`
(`synthetic_per_epoch`, `real_repeat`). On success Core immediately calls `/export` — the
artifact is ONNX + alphabet, not a torch checkpoint, and the service rejects an export that
diverges numerically from the model. `scripts/train-adr-ocr/` is only an eval harness.

Cancellation: `MlStudioTrainCancelRequest` (one variant for all tracks) → flag in `live_view`
+ service `POST /cancel/{job_id}`, and mesh command `MlTrainCancel` for remote training.
RF-DETR and LLM cancel by killing the child process (`RFDETR.train()` is one blocking call);
classifier and OCR cancel cooperatively per batch. A run orphaned by a Core restart is closed
by `reconcile_orphan_local_run` — the `register_local_run` marker distinguishes "we supervise
this" from "nobody watches this", with no time heuristics.

## Analytics (dashboard)

`www/js/modules/analytics.js` + `www/css/analytics.css` (screen id `analytics`, nav `nav.analytics`) is
the ONLY usage/metrics screen — it replaced `model-metrics.js` and `token-usage.js`. Tabs: overview /
users & groups / models / nodes & services / limits / billing; sticky filters (period + node + model)
auto-reload; row click = drill-down in the same tab with a breadcrumb. Data comes ONLY from
`model_metrics_rollup` (`ModelMetricsBody`); `token_usage_daily` is the AI-gateway enforcement source,
not a UI source. Quota/lease editing stays on `TokenUsagePayload` (`TokenQuotaWire.used_tokens` is
computed from the rollup). Wire rows carry Core-resolved names (`display_name`, `subtitle`,
`member_count`, node `last_seen_at`; `group_by=group` is keyed by group id, `group_by=hour` exists) —
the UI never shows a bare UUID/64-hex as a title. Node liveness = the later of
`sync_nodes.last_seen_at` and `peer_persisted.last_seen_ms` (mirrored into `sync_nodes` by the peer
registry writer); the local node is always "now". Numbers use `fmtCompact` (`12,4 tys / 121 mln`,
exact value in `title`), billing always exact (`fmtExact`/`fmtCurrency`) — helpers in `www/js/utils.js`.
Charts: `TfCartesianChart` (tooltip + crosshair on by default, one-time transform/opacity entry
animation honouring `prefers-reduced-motion`, stacked rounded tops, `narrow` slicing, tone `accent`
= `--tf-accent-3`). i18n namespace `analytics.*` must stay key-identical across all five locales.
E2E: `tests/e2e/analytics.spec.js` (fixture `tests/e2e/fixtures/analytics-seed.sql`).

Billing = `row_cost` in `dispatch/model_metrics.rs` over `DbModelPricing{prompt_per_1k,
completion_per_1k, embedding_per_1k, audio_per_min, image_each}`. Completion tokens come 1:1 from the
backend `usage` (streams force `stream_options.include_usage` upstream), so reasoning/thinking tokens
are billed at the completion rate — Core never estimates tokens. A successful chat/embedding call
without backend `usage` increments `model_metrics_rollup.usage_missing_count` (never for errors) and
the UI shows "cost incomplete: N requests without token data" next to `missing_pricing`; a backend
returning `usage: None` must be fixed at the backend (Codex parses `response.completed`).

## Admin Scheduler

`tentaflow-core/src/scheduler/`, state in SQLite (`scheduled_jobs`, `scheduled_runs`). Runs
addon tools via `AddonManager::call_tool`; the dashboard talks to it over the binary protocol
(`SchedulerBody`), not REST. Modes: `once` (RFC3339), `interval` (`30m`/`1h`/`1d`), daily
`cron` (`minute hour * * *`). UI in `www/js/modules/scheduler.js` (admin-only).

## Dashboard Settings

`Settings → Dostępy zewnętrzne`: external secrets (`hf_token`, `ngc_api_key`, container
registries) saved via the binary protocol with `is_secret`; listing returns `<redacted>` for
non-empty secrets. The vLLM recommender uses the stored `hf_token` to fetch `config.json` from
gated repos without persisting the token in the deployment config.

## Projekty (Project Studio)

`tentaflow-core/src/project_studio/` — native core module (NOT a WASM addon). Registry
(projects, members, creator grants, chats, notifications, `project_schedule_hints`) in
`<data>/projects.db`; per-project content in `<data>/projects/<id>/{project.db, files/<sha256>,
vectors/}` behind a bounded pool cache (`project_db.rs`: LRU max 16 open pools + idle sweeper,
migrations on every open, strict `validate_project_id` path guard, `checkpoint_all` at
shutdown). UI `www/js/modules/project-studio.js`, apps-home tile `projekty` WITHOUT
`requiresPowerUser`.

**Wire contract.** One `MessageBody::ProjectStudioBody(ProjectStudioPayload)`, currently 248
variants — strictly append-only (ciborium tags by variant NAME: never rename or reorder; golden
test `project_studio_wire_golden`). That is close to the 256-variant budget of the frame format,
so new features belong in a NEW sub-enum. Struct FIELDS are append-only the same way — add with
`#[serde(default)]` so peers that omit them still decode.

**Permissions.** Migration 119: `project_studio.read` (all roles) + `project_studio.admin`
(org_admin). Creating a project needs a per-user grant (`project_creator_grants`); in-project
roles owner/manager/editor/tester/viewer. Non-members get uniform NotFound (no existence leak);
an archived project is read-only (`require_active`). Chats are private per user — every chat
query is hard-filtered by `user_id`, with no admin bypass.

**Knowledge.** Native ingest (`ingest.rs` — office/text/code via `services/document/extract`,
PDF text layer via pdfium; scans/images are `skipped` until the vision pipeline is callable),
`split_into_chunks`, embeddings through the `rag-embeddings` alias, vectors under scope
`addon_id="ps-<project_id>"` namespace `passages` created AT the project directory
(`NamespaceManager::get_or_create_at`). Deliberately does NOT use the flow `store` node. Jobs:
cancel registry + `log_bus` progress + panic guard + semaphore of 2 concurrent ingests. Upload:
4 MiB chunks, 64 MiB per file.

**Chat.** System flow `ps-chat` (fixed UUID, `is_system=1`): trigger → project_knowledge →
conversation_history → llm streaming. Passages are pushed into `context.system_prompts` as
fenced DATA with `<<<PASSAGE>>>` delimiters (prompt-injection guard); citations persist in
`conversation_messages.citations_json`. `flows.is_system` (migration 118): system flows are
non-editable/non-deletable via the protocol, and sync NEVER trusts the wire `is_system` flag
(coerced to 0) — the local seed is the source of truth.

**Agent generation** (`generation.rs`) does NOT parse a "return JSON" answer — the agent writes
through a DURABLE SINK, builtin `core.project_case_save`. The target project/generation is not a
tool parameter but a server-minted binding in `envelope.meta["ps_generation"]`, injected via
`AgentRunManager::spawn` extra_meta, so the model can never redirect output elsewhere. Each case
is validated at save; a rejection is a `[TOOL_ERROR]` scoped to THAT case and the model repairs
it. Terminal watcher via `await_run` (30 min budget) + lazy reconciliation after restart.

**Manual testing.** Cases are versioned (`test_case_versions`, append-only) under optimistic
locking — ONE conditional `UPDATE … WHERE current_version = expected`, so a stale editor loses
instead of overwriting. A run SNAPSHOTS the case into `test_run_items`/`test_run_steps`, so a
later edit never rewrites history; a pool item is claimed by a single atomic
`UPDATE … RETURNING`.

**Automated testing.** Service `test-runner`
(`tentaflow-containers/tools/{_services,docker,python}/test-runner`, FastAPI, port 8093,
pytest/Playwright/Locust/httpx in killable subprocesses). The per-run host allowlist is enforced
INSIDE PYTHON — `executor/sandbox_net.py` wraps `socket.getaddrinfo` plus connect/sendto before
untrusted code is imported; the container itself runs `network_mode=bridge`. Core hardens it
with `SandboxLimits::test_runner` → bollard `HostConfig` (read-only rootfs, two tmpfs,
4 GiB / 2 CPU / 512 PID, `cap_drop: ALL`, `no-new-privileges`), applied only when
`engine.id == "test-runner"`. A PER-RUN sandbox is DEFERRED, hence: a unit case carrying
`build_profile_ref` becomes item 'blocked' (it would silently degrade to pytest in an empty tree
and report green), and a runner reporting `isolated: false` is refused unless an admin flips
`project_studio_allow_unisolated_runner`.

**Environments** (`environments.rs`) invert the `web_research` SSRF rule: a PUBLIC target is
auto-approved, a private/LAN/loopback one queues for an admin. The class is computed over EVERY
resolved address of EVERY allowlist host (one LAN entry makes the environment private; an
unresolvable name counts as private) and is RE-CHECKED at submit (`recheck_private`) — DNS may
move to 127.0.0.1 long after approval. The secret is SettingsCipher-encrypted, decrypted at
exactly one place (the submission body, never logged) and scrubbed from stored artifacts.
Sources: git clone via the system binary (a `repo_url` reaching a private address is a HARD
refusal — code sources have no approval queue), ZIP (`enclosed_name`, symlink refusal,
entry/byte budgets enforced while writing) and OpenAPI parsed by our own generic `paths` walk
(NOT `openapiv3`, which rejects a whole document over one non-conformant field).

**Schedules** ('once' RFC3339 / 'interval' / daily cron in an IANA zone via chrono-tz, DST
resolved explicitly) fire from a 30 s loop that never opens a project speculatively: the
registry `project_schedule_hints` (`next_run_at` per project, NULL = nothing pending) lets ONE
query pick due projects, otherwise every tick would thrash the 16-pool LRU. A due schedule is
claimed BEFORE firing by a conditional `UPDATE … WHERE next_run_at IS <stale value>`, and the
tick also runs `auto_runs::reconcile_running`. Gate outcomes are not interchangeable: 'blocked'
(missing/unapproved environment — an admin decision) and 'skipped' (previous run open, archived,
nothing executable, no runner) never advance the failure breaker; 5 consecutive 'error' outcomes
set `auto_disabled` (resume is manual only).

**ML Studio link** (`ml_link.rs`): ONE-WAY project→ML permission mirror (5 project roles →
editor/viewer). It acts as the ML project OWNER and records what IT granted in a project setting
(`ml_link_granted:<link_id>`), so it never revokes someone else's membership. The Power-User
wire gate is not waived; the 10 READ handlers in `dispatch/ml_studio.rs` are
`#[policy(UserSession)]` because each carries its own membership check (every MUTATING ML
handler stays `PowerUser`).

**Export/import** (`archive.rs`): the db snapshot is `VACUUM INTO` after a TRUNCATE checkpoint
(a plain file copy would archive a file missing its own `-wal` commits); the snapshot blanks
both `secret_enc` columns, resets every environment to 'pending' and DISABLES every schedule.
The manifest carries per-file sha256, verified after extraction; import enforces
`enclosed_name`, refuses symlinks, whitelists entry prefixes and caps entries/bytes. Vectors are
moved verbatim ONLY when the `rag-embeddings` alias resolves to the SAME target model and the
metric/dim/field fingerprint matches — otherwise the import re-indexes from archived blobs.

**UI conventions** pinned by `mockups/projekty-20260723/`: one toolbar row (searchbox + selects,
primary action right-aligned via `.ps-toolbar-spacer`), a `.ps-table-footer` summary under every
list, two-line table cells (`tf-table__cell-title` + `tf-table__cell-sub` with the short id), and
KPI rows built from full `tf-stat-card`s in `.ps-kpi-grid`.

## Compliance Core

`tentaflow-core/src/compliance/` — shared core layer for GDPR/RODO, AI audit, retention, ROPA,
DSAR, consents, DPIA, breach register. Migration `compliance_core_foundation` creates the
canonical tables and seeds per-org defaults; UI-visible text uses `*_translations` fields
validated by `json_valid` (seed must include at least `pl` + `en`). `compliance_ai_events` holds
one AI call/session and links to the `audit_log` chain via `audit_log_id` (prompts, responses,
sources, tool calls stay in dedicated compliance AI tables). `AiGateway` is the central entry
for blocking + streaming chat and addon `llm_generate`. AI-audit retention resolves via
`compliance_retention_policies` and cannot be shorter than 183 days. Admin protocol:
`MessageBody::ComplianceAdminBody` + `tentaflow-protocol/src/compliance.rs` (CBOR carries
summaries, never prompt/response bodies). Access needs `compliance.read`; `org_admin` and `dpo`
also get `compliance.write`.

## Native libraries

Build: `scripts/native-libs/build-all.sh` (Linux/macOS), `build-all.ps1` (Windows),
`build-all-android.sh` (NDK cross-build → `native-libs/android-{arm64,armv7,x86_64}`). Update
sources with `--update` / `-Update`. Platform layout: `include/`, `lib-static/`, `lib-dynamic/`,
`manifest.toml`; static is preferred, dynamic libs are copied next to the binary by
`tentaflow/build.rs`.

- **Nothing under `native-libs/<platform>/` is committed** (Git LFS was abandoned — it exceeded
  limits and blocked push). Everyone builds locally; `.gitignore` keeps only `README.md`.
- Downloaded sources go OUTSIDE the repo to `TENTAFLOW_NATIVE_CACHE` (default
  `${XDG_CACHE_HOME:-$HOME/.cache}/tentaflow-native-libs`) — **not `/tmp`**: a RAM tmpfs
  truncates extraction on small-RAM machines → CMake "Parse error … bad character".
- llama.cpp: pinned `LLAMA_CPP_REF=6b80c74f` so everyone shares one prebuilt (`origin/master`
  for fresh, `vendored` for the old tree). Variant via `LLAMA_CPP_NATIVE_VARIANT` (default
  `multi` = cuda + vulkan + cpu on a Linux/Windows machine with a VISIBLE NVIDIA GPU, otherwise
  vulkan + cpu). Apple is a separate branch entirely — macOS/iOS resolve to **metal** and nothing
  else, so none of the CUDA/ROCm/Vulkan rules below apply there.
  **On Linux/Windows, autodetection never picks ROCm**: NVIDIA runs on CUDA, everything else on the portable Vulkan
  backend. ROCm is still built on request — `LLAMA_CPP_BACKENDS=rocm` — but never by accident,
  because CUDA and HIP ggml backends can NEVER share one static object (same symbols, one
  `ggml_backend_cuda_reg`), so an auto-pick between them turns "which driver answered at build
  time" into a different artifact. A CUDA toolkit alone is NOT the signal either: a box that
  keeps `/opt/cuda` with no NVIDIA card in the slot builds vulkan. Force any of it with
  `LLAMA_CPP_BACKENDS=cuda|cpu|vulkan|rocm`. The
  build scripts find `nvcc`/`hipcc` outside PATH (`/usr/local/cuda`, `/opt/rocm`, `CUDA_HOME`)
  and wipe a variant's `lib-static`/`lib-dynamic` dirs before install so a stale
  `libggml-cuda.a` cannot get linked next to a fresh `libggml-hip.a`. Local patches come from
  `scripts/native-libs/patches/llama-cpp/` (current one turns the fused Gated Delta Net
  auto-detect `SIGABRT` on Qwen3.6/MTP into a warning).
- Whisper autodetection on Linux/Windows follows the same rule (`WHISPER_CPP_BACKENDS`); on Apple
  it is metal, and `inference-whisper` is not linked there at all. Building ROCm on purpose
  needs an architecture pin and a real Clang, because CMake >= 4 REFUSES the `hipcc` wrapper as
  the HIP compiler: `HIPCXX=/opt/rocm/llvm/bin/clang++ CMAKE_HIP_ARCHITECTURES=gfx1201
  LLAMA_CPP_BACKENDS=rocm ./scripts/native-libs/build-all.sh` (the scripts now default `HIPCXX`
  to the ROCm clang, falling back to `hipcc` only when no clang is next to it).
- CUDA NV12/RGB preprocessing for the zero-copy ORT path is opt-in via `gpu-cuda` or
  `vision-cuda`; a plain AMD/Intel build never invokes `nvcc` and preprocesses on the host. The
  portable WGPU/Vulkan backend is Burn's default for the main vision pipeline
  (`inference-supertonic` no longer forces ORT for vision), while `gpu-cuda` keeps the
  ORT/TensorRT/CUDA path on NVIDIA.
- Android specifics: GGML builds use `GGML_OPENMP=OFF` (no `libomp.so` runtime);
  `build-rust.sh` detects the NDK, picks ABIs from the attached phone via `adb`
  (`ANDROID_ABIS=auto`, `all` for everything), fetches the GStreamer Android SDK, and copies
  `libwhisper_tf.so` + `libc++_shared.so` into `app/src/main/jniLibs/<abi>/`.
  `tentaflow-mobile/android/gradlew` bootstraps Gradle 8.2.1 (+ Temurin JDK 17 when the system
  Java is too new). `tentaflow-mobile/core/build.rs` links the static GStreamer archives
  (`ffi`, `pcre2-8`, `gmodule-2.0`, `iconv`, `intl`) so `libtentaflow_mobile.so` has no
  unresolved `ffi_type_void` at `dlopen`.

### INVARIANT: symbol isolation (MANDATORY for every vendored native library)

Every self-contained native library (whisper, zvec, …) MUST export ONLY its own public API
(`whisper_*`, `zvec_*`) and HIDE everything bundled (protobuf, abseil, RocksDB, Arrow, ggml).
Otherwise two copies of the same C++ library (e.g. protobuf in zvec AND in the binary's ONNX
layer) interpose through the Linux dynamic linker and corrupt the heap during static init
(`corrupted size vs prev_size`, crash before `main`; macOS's two-level namespace protects it,
but we isolate anyway).

Mechanism: Linux `-Wl,--version-script` with `{ global: <name>_*; local: *; }`; macOS
`-Wl,-exported_symbols_list` with `_<name>_*`; Windows `.def`. See `scripts/build-zvec.sh` and
`build-whisper-cpp.sh`.

TRAP: deploy runs the binary with `LD_LIBRARY_PATH=target/<profile>`, which outranks the rpath
to the vendor dir — `tentaflow/build.rs` MUST copy the fresh vendored `.so/.dylib` into
`target/<profile>` (full copy, not a hardlink), or the stale copy shadows the new one.
Verify: `nm -D --defined-only <so> | grep -ic protobuf` = 0.

whisper is the reference case: whisper.cpp and llama.cpp always coexist in the binary (every
`gpu-*` feature pulls `inference-llamacpp`) and each vendors its own ggml version under
identical `ggml_*` symbol names. whisper + its ggml are linked into one isolated
`libwhisper_tf.so` (ggml_* hidden), `whisper-rs-sys/build.rs` links that dylib dynamically, and
`tentaflow/build.rs` copies it flat next to the binary (`$ORIGIN`).

### Inference wrappers

`tentaflow-wrappers/` holds our own wrappers for llama.cpp / whisper.cpp, defining the config
contract and the `native-libs` artifact mapping, so we can use upstream features (`mtp`,
`ngram-simple`) that high-level bindings block. The crate has NO tokio dependency.

- `llama_engine.rs` — continuous-batching engine (one model, one ctx, many sequence slots,
  per-slot anti-hang). `SpeculativeMode`: `Off`, `NgramSimple`, `Mtp { n_max }` (self-speculative
  through the model's own MTP/NextN head — no duplicated weights; the scheduler creates a second
  `ctx_dft` on the same model and drives `common_speculative`). Both contexts live only in the
  scheduler thread. KV rollbacks of `ctx_dft` are load-bearing: to `base_pos` after `draft()` and
  to the new free position after acceptance — without them M-RoPE rejects the batch. MTP only
  pays off when the GPU is not saturated (a single or rare stream); with several parallel
  sequences the draft overhead outweighs the gain.
- Anti-hang contract: `submit_with_sink` takes a `Box<dyn EngineSink>` and the scheduler calls
  `sink.try_send` from its own thread (no thread per request). A `Full` token is deferred to the
  slot's `pending`, including the terminal token, and `release_slot` only frees the slot after
  the tail drains — no blocking `send`. `EngineConfig.stream_stall_timeout` (deploy
  `stream_stall_timeout_secs`, default 60 s) is a DELIVERY-PROGRESS deadline: a slot whose
  `pending` never shrinks (consumer alive but mute) is force-released with `FinishReason::Error`,
  so exhausting `queue_capacity` cannot silently wedge admission.
- `inference/llamacpp.rs` (`LlamaCppEngine`) plugs the engine into core. The engine lives in
  `RwLock<Option<Arc<LlamaEngine>>>`; `generate`/`generate_stream` take a SHORT read lock, clone
  the `Arc` and release it BEFORE `submit` — no lock is ever held during generation. Core
  supplies `StreamSink`/`CollectSink` over `tokio::mpsc`. `StreamToken` carries real
  `finish_reason` + `error` + `prompt_tokens`, and `generate` measures `tokens_per_second` from
  the first token (TTFT). Deploy params → `EngineConfig`: `n_seq_max` from
  `n_parallel`/`max_concurrency` (default 8), `ctx_per_seq` = ctx_size, `n_batch` = batch_size,
  speculative from `speculative_method`/`num_speculative_tokens`/`size_ngram`/`size_mgram`.
- The engine is generation-only, so `LlamaCppEngine::embeddings` uses a separate, lazily created
  `LlamaRuntime` (context with `embeddings=true`). `LlamaRuntime` is ONLY the embeddings path —
  it keeps `load`/`metadata`/`embeddings`/`tokenize`/`context` plus helpers shared with the
  engine. Full unification is deliberately deferred.
- Smoke tests: `examples/llama_smoke.rs` (`--metadata-only` reads GGUF metadata and detects MTP
  via `*.nextn_predict_layers`; without it, loads through `LlamaEngine`),
  `examples/llama_engine_smoke.rs` (`--speculative off|ngram|mtp`, per-request tok/s, anti-hang
  regressions `--slow-consumer` / `--drop-mid` / `--silent-consumer` / `--queue-overflow`),
  `examples/whisper_smoke.rs`. Core integration test:
  `tentaflow-core/tests/llamacpp_engine_e2e.rs` (`#[ignore]`, env `TENTAFLOW_LLAMA_TEST_MODEL`).
- `inference-whisper` is ALWAYS built on non-Apple platforms (Linux/Windows in
  `tentaflow/Cargo.toml`, Android in `tentaflow-mobile/core/Cargo.toml`) — the `whisper` STT
  engine must exist at runtime there, it is not opt-in. Apple (macOS/iOS) does NOT link
  whisper.cpp (STT goes through MLX-whisper / native Swift); wired per-target via a separate
  `[target.'cfg(...)'.dependencies]` block.
- A llama.cpp deploy from a HuggingFace GGUF repo must name a single `.gguf`
  (`config_json.model_file`) or a preset with `quantization` — the downloader must not pull
  every quantization in the repo.

## tentaflow-infer (FORGE)

Independent inference-engine project (own Cargo workspace, NOT part of the main binary): Rust
systems layer + **Mojo GPU kernels** (AOT → PTX + manifest, zero Mojo runtime in the server;
ADR-0001). Crates: forge-types/hal (CUDA via cudarc, VRAM arenas, CUDA graphs) / formats
(GGUF + safetensors + NVFP4, CPU golden dequant) / tokenize / kernels (PTX registry + typed
launchers, golden GPU tests) / state (paged KV + radix tree, shared by both paths) / engine
(forward pass, scheduler) / server+cli (OpenAI API).

Docs: `tentaflow-infer/docs/SPEC.md`, `docs/PLAN.md`, `docs/STATUS.md`, target architecture
`docs/ARCHITEKTURA_DOCELOWA.md`, Mojo 1.0b API quirks `kernels/mojo/MOJO_NOTES.md`,
benchmarks `docs/BENCH_*.md` (`BENCH_NVFP4_VLLM.md`, `BENCH_QWEN35_MTP_NVFP4.md`).

Kernel toolchain: `cd tentaflow-infer/kernels/mojo && pixi run mojo build_kernels.mojo` (pixi
env is gitignored; artifacts in `build/<arch>/` ARE committed). The builder compiles the kernel
catalog in isolated Mojo units, splits a unit after a compiler offload error, stages PTX/cubins/
manifest on the same filesystem and publishes the whole arch directory with an atomic
`RENAME_EXCHANGE` — a late failure leaves the previous set untouched. PTX artifacts need
`sm_80+`.

**Direction.** A second layout (`docs/ARCHITEKTURA_DOCELOWA.md`) treats a model as a SEQUENCE OF
OPERATIONS (`forge-graph`): `forge-model` emits `Op` and knows neither HAL nor kernels, and three
executors compute the same contract — Metal, a host f32 oracle (runs without an accelerator) and
`CudaExec`. Weight form and KV paging belong to the EXECUTOR, not the model. This is the only
target path; `forge-engine` is to be rewritten onto it and disappear (order and measurement bar
in §8 of that doc). Until the measurement matches, `forge-engine` stays in production.

**Scope limits (do not overstate).** The HAL supports CUDA only. AMD/ROCm, Metal and native
Blackwell FP4 instructions are NOT implemented — a capability fallback is not backend support.
Mojo sources keep a split that allows future AMDGPU/Metal codegen, but neither backend is wired
or tested. Everything below was verified only on CUDA / RTX 4090.

### NVFP4 / FP8 path

- For a catalog checkpoint, an unset `FORGE_GEMM` auto-attempts hybrid FP8 prefill: Mojo kernels
  repack NVFP4 Q/O/gate/up/down and the single F16 `lm_head` to FP8 on GPU. `fp8mod-ffn` forces
  the attempt; any other explicit value disables auto. Source NVFP4 stays resident for decode;
  K/V are never converted.
- Conversion may only start after checking device capability, supported shapes, artifact
  completeness and free VRAM. Any unmet condition leaves the whole layer on the existing path.
- `FORGE_NVFP4_CT_LAYOUT=auto` (default) picks S0 only after those checks pass, else row-major.
  Small-batch BM16 is likewise automatic; `FORGE_NVFP4_CT_BM16=0` disables it explicitly.
- `serve`, `run`, `embed` and `ppl` always use `RowMajor36`. `TileN128K64` is an explicit
  `bench`-only comparison mode via `FORGE_BENCH_NVFP4_TILE=1` and requires
  `SpeculationKind::Off` with one active sequence.

### Speculative decoding

- `forge-engine::speculation` defines the shared `Proposer` contract, typed `DraftTree`/
  `DraftNode`, `SpeculationCoordinator`, cascade composition and per-proposer acceptance stats.
  Nodes carry source, `proposal_logprob` and `conditional_confidence`, so the same contract will
  later cover greedy and lossless stochastic acceptance.
- Working today: host `NgramProposer`, native MTP/NextN, and their priority router for the dense
  hybrid `qwen35` GGUF. Native MTP splits `nextn_predict_layers` out of the trunk and loads their
  NVFP4/Q8_0/F32 without a second copy of the target; proposer, full-draft verification and
  DeltaNet/KV checkpoints run on GPU through Mojo kernels. Server flags: `--speculative mtp`,
  `mtp:2`, `mtp:3`, `mtp+ngram:2|3` (a complete n-gram draft bypasses the MTP proposer; MTP state
  catches up on the accepted prefix). `mtp` and `mtp:3` hold K=3; K=2 is a fallback only on
  insufficient context or KV pool.
- NOT implemented: `draft-model`, `eagle`, `dflash`, `dspark` return `Unsupported`; tree
  verification, sampling beyond greedy-exact, PARD and suffix are absent.
- Native MTP is greedy-exact. Default `max_active=1`; an explicit `max_active > 1` goes through
  an atomic startup preflight. The scheduler pairs two pure-MTP requests with the same K into
  native B2 (`FORGE_NATIVE_MTP_B2` accepts only `0`/`1`, default on); different K, `mtp+ngram`,
  tiering, an incomplete pair or an unmet kernel contract fall back to serial B1. Target DeltaNet
  and draft MTP keep isolated per-sequence state under a shared lease; a restore/rollback error
  poisons and quarantines both leases.
- Admission reserves a logical future-KV-page budget per active sequence, enforces
  `max_pages_per_seq` and runs an atomic whole-batch growth preflight. A bounded queue window
  (`2 * max_active`, clamped 2–16) skips requests temporarily blocked on KV, with aging against
  starvation. Pinned borrowed-prefix pages are accounted conservatively, which can delay
  admission despite physical KV sharing.
- `forge-formats` parses `forge-speculation.json` for neural proposers (target, fingerprints,
  tensors, features, dtype/quantization, sampling, calibration, separate code/weight licenses).
  `SpeculationManifest::load` confines artifacts to the manifest directory and verifies each
  file's SHA-256; fingerprint comparison against the live target lands with the neural runtime.

### Hybrid prefill and MTP catch-up

- The hybrid target prefills in matrix chunks on GPU; dynamic DeltaNet kernels handle full
  chunks and commit recurrent state per chunk, with no sequential trip through the CPU.
- Layer-major runs full GEMM projections once per layer in a lazy arena up to P4096 and is the
  DEFAULT for compatible models (`FORGE_HYBRID_LAYER_MAJOR_PREFILL=0` disables it
  diagnostically). It sizes the arena to the current activation budget: a lone prefill uses all
  capacity, an active decode caps the scheduler quantum at T1024 and interleaves segments.
- Layer-major prefill is a TRANSACTION covering target + MTP catch-up: before mutating it
  checkpoints all DeltaNet states/windows and the partial KV tail via D2D copies into existing
  verifier buffers, and any error restores state, KV, page map, tokens and sequence lengths. The
  MTP checkpoint stays live through profile-end registration and the final GPU sync; the commit
  is validated first but applied as the last error-free change.
- Native MTP with a shared target embedding catches up the whole accepted prefix in ONE GPU
  batch; models without a compatible shared embedding keep the sequential variant.
- Hybrid models with native MTP reserve 1152 MiB of activation pool to hold prefill buffers,
  verifier buffers, DeltaNet states and batched catch-up at once.
- Kernel selection is capability-gated, and every gate has a bit-exact fallback: persistent
  DeltaNet scan only on NVIDIA / warp32 / `d_state=128` / T>128 with the artifact present
  (`chunked` forces the fallback); `FORGE_HYBRID_LAYER_MAJOR_ATTN` defaults to Mojo FA HD256
  (`exact`/`prefill` force older variants); Flash Attention HD256 defaults to the bit-exact K16
  variant, with the inexact K32 only via `FORGE_HYBRID_FA_KEY_TILE=32`;
  `FORGE_HYBRID_LAYER_MAJOR_DELTA_PREPARE=tiled` opts into tiled DeltaNet prepare. A missing
  artifact always degrades to the previous kernel rather than failing.
- Target KV keeps a compact `global_layer -> kv_layer` map and allocates slabs only for
  `Attention` layers, so a hybrid model pays KV only for its attention layers; the separate MTP
  cache keeps a single-layer identity map.
- DeltaNet D128 state uses the `ValueKey` layout, chosen once at pool creation — same byte size
  as `KeyValue`, and an incomplete artifact set or unsupported warp geometry selects the portable
  `KeyValue` before allocation.
- `FORGE_MTP_DRAFT_HEAD=nvfp4` (optional) repacks the shared Q8_0 head into a separate NVFP4 GGUF
  copy used ONLY for MTP proposals; the target verifier keeps the original Q8_0.
- Known limit: the full builder blocks FP8 paths needing PTX 8.4 while Mojo emits PTX 8.1.

### GPU histogram sampling

Active repetition / frequency / presence penalties are applied by a single histogram kernel,
followed by the existing parallel argmax or top-k selection. The default greedy path without
penalties launches no extra kernel and does no extra allocation or sync.

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
   `<input type="file">`.) Component styling lives in `css/controls.css` — it is the ONLY
   sheet adopted into shadow roots, so markup injected into a `tf-table` cell can never use a
   feature stylesheet's classes. `tf-table selectable="multi"` renders a checkbox in the first
   cell of every row and emits `row-select`; a plain row click stays `row-click` (open), and
   `tf-segmented` options take an `icon` attribute. Filter/action bars use the shared
   `.tf-toolbar` class (+ `.tf-toolbar-spacer` to push actions right): `tf-select`/`tf-input`
   default to `width:100%`, so a bar without it stacks every control on its own row.
8. **Plural forms go through i18n, never string concatenation.** `{count|forma1|forma2|forma3}`
   in a translation picks the right form (Polish needs all three, other languages two), so
   "1 przypadków" cannot happen. Every `project_studio` key exists in all five locales — the
   parity check is a plain key-count comparison across `www/i18n/*.json`.

## gstack & skill routing

For web browsing use the `/browse` skill — never `mcp__claude-in-chrome__*`. Other gstack
skills (if installed): `/qa`, `/qa-only`, `/review`, `/ship`, `/investigate`, `/design-review`,
`/land-and-deploy`, `/canary`, `/benchmark`, `/codex`, `/cso`, `/careful`, `/freeze`, `/guard`.

When a request matches a skill, invoke it FIRST: bugs/errors → investigate; ship/deploy/PR →
ship; QA → qa; code review → review; architecture review → plan-eng-review; design polish →
design-review.
