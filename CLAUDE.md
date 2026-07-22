# CLAUDE.md

Guidance for Claude Code (claude.ai/code) working in this repo.

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

macOS 26+ (Xcode 26) split the Metal compiler into a separate component. Without it,
`xcodebuild` builds a broken `mlx.metallib` and EVERY MLX model returns gibberish (wrong
GPU logits) with no build error. `setup.sh` installs it; `tentaflow/build.rs` fails loudly
if missing. Fix: `xcodebuild -downloadComponent MetalToolchain` + drop the stale metallib
(`rm -rf tentaflow-desktop/macos/swift/MLXBridge/build-xcode`). MLXBridge builds via
`xcodebuild -skipMacroValidation` (SwiftPM CLI can't compile Metal shaders).

`tentaflow-core` features — default = `inference-whisper` + `camera`. Key opt-ins:

| Flag | Purpose |
|------|---------|
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
- `GET /models/manifest/<bundle>` / `GET /models/file/<bundle>/<name>` — vision model-bundle
  sharing between instances. Auth: signed URL OR `Authorization: Bearer <api-key>` with an
  explicit `('model_bundle', <bundle_ref>)` allow scope (default-DENY, general keys only) —
  works between UNPAIRED instances. API-key manifests return token-less file urls; the client
  repeats the Bearer header. Pull side: deploy wizard "Custom" tab → config `vision_bundle_url`
  + `vision_bundle_api_key` (encrypted like `api_key`), fallback settings
  `vision_bundle_base_url` + `vision_bundle_api_key`.

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
output, plus
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
which nodes receive which resources. Startup runs `ensure_default_core_sync_policies` +
`ensure_trusted_nodes_in_sync_identity`: global core resources get a default
`replicated_by_permission` policy and active `trusted_nodes` are materialized into
`sync_nodes`, so Flow Builder, shared settings, identity and RBAC reach the outbox without
hand-seeding policies.

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
Flow blocks + app panels), `memory`, `embeddings-chunker`, `notes` (Rust, UI catalog v1:
per user/group/org notes with ACL-guarded auto-graph — SQLite source of truth +
`graph_outbox` → Cozo `notes_kg` + zvec namespaces, hybrid RRF search with streamed LLM
answer, share modal on `directory.read` host fns, STT dictation, flow blocks + LLM tools).
The legacy built-in notes screen/protocol was removed (protocol `SCHEMA_VERSION` 21 —
old/new binaries reject each other on handshake; rebuild all mesh nodes together).

Host fns `directory_users/groups/roles/org_v1` (scope `directory.read`) expose the org
identity catalog to addons. Rust addons get typed UI catalog v1 bindings via
`scripts/gen-rust.sh` → `addon-sdk/sdk/src/ui_v1/` (re-exports of `tentaflow-sdk-spec`
components; same codegen pipeline as C#/Python). `RelationGraph` (0x0703) has a canvas
renderer (`tf-relation-graph`); audio-capture uploads are bound host-side to the uploader
(`source=audio_capture` docs deny cross-user `document_get/delete/list`).

`[[network_rule]].host` supports exact hosts, `*.domain` subdomain wildcards and `*` for
public-web addons. Wildcards still require explicit admin approval, keep the declared port,
and remain behind the host HTTP SSRF guard (public destinations only). Exact-host rules are
the allowlist: once approved they may target ANY address, including private/LAN/loopback —
admin explicitly approving `192.168.x.x:port` in the Network tab is the intended way to reach
local services (e.g. a LAN MCP server). When a destination matches both, exact beats wildcard.
`http.request` never follows redirects (addon receives the raw 3xx).

`tentaflow-core/src/web_research/` is the central public-web research service for addons:
configurable search providers (`searxng`, `duckduckgo`, `brave`, `tavily`), public URL reading with DNS
pinning/redirect revalidation/body limits, generic HTML/text extraction and batch
`read_search_results`. Addons call it through SDK `web_*` wrappers and need `web.research`.
When a search request omits `provider`, the host function resolves a running local
`searxng` service from `services.endpoint_url` and marks it as an internal Core provider.
If no local service exists, Core selects a visible remote `searxng` service from the mesh
registry and sends a trusted `MeshCommandType::WebResearch` command to that owning node;
the remote node executes the request against its own local endpoint and returns the serialized
`WebResearchResponse`. This keeps phones and other mobile nodes from needing direct HTTP
access to another node's loopback SearXNG port. If no SearXNG exists anywhere, Core falls
back to the public DuckDuckGo HTML provider. Page URLs returned by search are still read through
the public URL SSRF guard.

Page reading is not a simple tag-stripper: `reader.rs` fetches only public readable content types
with redirect revalidation and body limits, then `extract.rs` runs `readability` (default features
disabled, no independent HTTP client) against the already fetched HTML. If Readability cannot
produce useful text, Core falls back to local semantic block scoring (`article`/`main`/content
classes, link-density penalty, boilerplate cleanup). Responses include extraction method,
character count, word count and `quality_score` so flows/LLMs can rank or reject weak pages.
Addon calls use Browser Renderer for `mode="auto"` when a local or visible remote
`browser-renderer` service exists; `mode="browser"` requires it and `mode="static"` keeps the
pure HTTP/readability path. `read_search_results` now treats `search_limit` as the candidate pool
and keeps reading until it gets `read_limit` successful pages or runs out of search results.
Bundled addon `deep-research` is only the LLM tool / Flow Builder facade over that SDK.

## Service Containers

`tentaflow-containers/tools/_services/searxng.toml` registers SearXNG as an infra service
for public-web search. It has a Docker deployment based on the official
`searxng/searxng` image and a native `python-bundle` deployment from the upstream git repo.
Both variants enable JSON search output for `web_research`; iOS does not run this service
natively because SearXNG is a Python web application, not an embedded Rust engine.

`tentaflow-containers/tools/_services/browser-renderer.toml` registers the Playwright
Chromium renderer used for JS-heavy page reading. Docker runs from
`tools/docker/browser-renderer/` on the official Playwright Python image; native deploy uses
`tools/python/browser-renderer/` as a `python-bundle`. The service exposes `/render`,
`/contexts`, and `/health`, keeps one persistent headless browser context per `user_id`,
evicts idle contexts by `BROWSER_RENDERER_CONTEXT_TTL_SECONDS`, and caps active contexts with
`BROWSER_RENDERER_MAX_CONTEXTS`. Each profile has isolated cookies/localStorage/sessionStorage.
The renderer validates public URLs before navigation and aborts private/local subresource
requests inside Playwright routing, so Core can use it without exposing local node networks to
web pages.

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

## Compliance Core

`tentaflow-core/src/compliance/` — shared core layer for GDPR/RODO, AI audit, retention,
ROPA, DSAR, consents, DPIA, breach register. Migration `compliance_core_foundation` creates
the canonical tables and seeds per-org defaults; UI-visible text uses `*_translations` fields
validated by `json_valid` (seed must include at least `pl` + `en`). `compliance_ai_events`
holds one AI call/session and links to the `audit_log` chain via `audit_log_id` (prompts,
responses, sources, tool calls stay in dedicated compliance AI tables). `AiGateway` is the
central entry for blocking + streaming chat and addon `llm_generate`: it starts the event,
records prompt/response/tool calls and the final `audit_log` entry. AI-audit retention is
resolved via `compliance_retention_policies` and cannot be shorter than 183 days. Admin
protocol uses `MessageBody::ComplianceAdminBody` + `tentaflow-protocol/src/compliance.rs`
(CBOR carries category/retention/AI-event summaries, never prompt/response bodies). Admin
access needs `compliance.read`; `org_admin` and `dpo` also get `compliance.write`.

## Native Libraries

- `scripts/native-libs/build-all.sh` (Linux/macOS) i `scripts/native-libs/build-all.ps1`
  (Windows) wykrywają platformę i budują natywne zależności do `native-libs/<platform>/`.
- Android ma osobny cross-build producer: `scripts/native-libs/build-all-android.sh`.
  Buduje `zvec`, `llama.cpp` i izolowane `whisper.cpp` przez Android NDK do
  `native-libs/android-arm64`, `native-libs/android-armv7` i `native-libs/android-x86_64`.
  Domyślny wariant llama/whisper to `multi` CPU, zgodny z `LLAMA_CPP_NATIVE_VARIANT`
  i `WHISPER_CPP_NATIVE_VARIANT` w sys-crate'ach. Androidowe buildy GGML mają
  `GGML_OPENMP=OFF`, żeby nie wymagać dodatkowego runtime `libomp.so`. Skrypt kopiuje też `libc++_shared.so`;
  `tentaflow-mobile/android/scripts/build-rust.sh` kopiuje `libwhisper_tf.so` i
  `libc++_shared.so` do `app/src/main/jniLibs/<abi>/`.
- `tentaflow-mobile/android/scripts/build-rust.sh` jest samowystarczalnym wejściem dla
  Androida: wykrywa NDK w standardowych lokalizacjach, wybiera ABI z podłączonego
  telefonu przez `adb` (`ANDROID_ABIS=auto`, domyślnie), pobiera GStreamer Android SDK
  do `TENTAFLOW_NATIVE_CACHE` gdy go brakuje, i wywołuje `build-all-android.sh` dla
  brakujących native-libs. `ANDROID_ABIS=all` buduje wszystkie ABI.
- `tentaflow-mobile/android/gradlew` jest repo-local bootstrapem Gradle: pobiera Gradle
  8.2.1 i, gdy system ma zbyt nową Javę, Temurin JDK 17 do `TENTAFLOW_NATIVE_CACHE`.
  `scripts/setup.sh` wywołuje ten bootstrap, więc Android APK nie wymaga ręcznej
  instalacji systemowego Gradle/JDK.
- `tentaflow-mobile/core/build.rs` dopina na Androidzie statyczne archiwa GStreamer SDK
  (`ffi`, `pcre2-8`, `gmodule-2.0`, `iconv`, `intl`), żeby `libtentaflow_mobile.so`
  nie zostawiał niezwiązanych symboli typu `ffi_type_void` przy `dlopen` na telefonie.
- Źródła pobierane przez skrypty trafiają poza repo do `TENTAFLOW_NATIVE_CACHE`
  (domyślnie `${XDG_CACHE_HOME:-$HOME/.cache}/tentaflow-native-libs` — trwały cache na
  dysku, NIE `/tmp`: tmpfs w RAM urywa rozpakowanie na małym RAM-ie → CMake „Parse
  error ... bad character"), więc repo przechowuje **tylko skrypty**.
  Cała zawartość `native-libs/<platform>/` (nagłówki, biblioteki, `manifest.toml`)
  jest generowana lokalnie i NIE jest commitowana — `.gitignore` ignoruje `native-libs/*`
  poza `README.md`. Każdy buduje u siebie: `./scripts/native-libs/build-all.sh`.
- Układ platformy: `include/`, `lib-static/`, `lib-dynamic/`, `manifest.toml`.
  Biblioteki statyczne są preferowane, a dynamiczne są kopiowane przez `tentaflow/build.rs`
  obok budowanej binarki z `native-libs/<platform>/lib-dynamic`.
- Aktualizacja źródeł: uruchom `build-all.sh --update` albo `build-all.ps1 -Update`.
- `build-llama-cpp.sh` domyślnie buduje **pinowany commit** `LLAMA_CPP_REF=6b80c74f`
  (świeży master z 2026-06-06 — Qwen3.6 MTP/NextN wymaga nowego `llama.cpp`), żeby
  wszyscy mieli identyczną wersję jako prebuilt w `native-libs`. Świeży master:
  `LLAMA_CPP_REF=origin/master`. Stare vendored źródła: `LLAMA_CPP_REF=vendored`
  (drzewo `vendor/crates/*/llama.cpp/` NIE jest w repo — patrz `.gitignore`).
- Biblioteki NIE są w repo (porzucony Git LFS — przekraczał limity i blokował push).
  Każdy buduje je lokalnie przez `build-all.sh`; pliki zostają na dysku, ale są
  gitignorowane. Wariant llama.cpp wybiera
  `LLAMA_CPP_NATIVE_VARIANT` (domyślnie `multi` = cuda+rocm+vulkan+cpu w jednym; linkowanie
  `multi` wymaga obecności WSZYSTKICH trzech runtime'ów — pod różne GPU buduj warianty
  jednobackendowe przez `LLAMA_CPP_BACKENDS=cuda|cpu|vulkan|rocm`).
- `build-llama-cpp.sh` aplikuje lokalne patche z `scripts/native-libs/patches/llama-cpp/`.
  Obecny patch usuwa `SIGABRT` w auto-detekcji fused Gated Delta Net dla Qwen3.6/MTP
  na CUDA: nieznana nazwa tensora wyłącza fused GDN i loguje warning zamiast ubijać proces.
- `tentaflow-wrappers/` centralizuje własne wrappery dla `llama.cpp`, `whisper.cpp`
  i kolejnych silników. Ten crate definiuje kontrakt konfiguracji oraz mapowanie
  artefaktów z `native-libs`, żeby stopniowo odchodzić od high-level bindingów
  blokujących nowe funkcje upstreamu, m.in. `mtp` i `ngram-simple` w `llama.cpp`.
- `tentaflow-wrappers/examples/llama_smoke.rs` służy do szybkiego testu GGUF:
  `--metadata-only` czyta metadane bez ładowania wag i wykrywa MTP po
  `*.nextn_predict_layers`; bez tej flagi ładuje model PRZEZ SILNIK (`LlamaEngine`,
  jedyna ścieżka generacji) i sprawdza streaming pojedynczego requestu. MTP bez
  głowy nextn degraduje do `Off` już z nagłówka GGUF (`mtp_layers==0`).
- `tentaflow-wrappers/src/llama_engine.rs` to silnik continuous batching nad
  llama.cpp (jeden model, jeden ctx, wiele slotów sekwencji, anty-hang per-slot).
  `SpeculativeMode` ma trzy tryby: `Off`, `NgramSimple` (drafter ngram bez modelu
  draftującego) i `Mtp { n_max }` (self-speculative przez głowę MTP/NextN W TYM
  SAMYM modelu — zero duplikacji wag). Dla MTP scheduler tworzy drugi kontekst
  `ctx_dft` na tym samym modelu (`ctx_type=LLAMA_CONTEXT_TYPE_MTP`, `n_rs_seq=0`)
  i wpina go do shimu `common_speculative`. Pętla MTP: `llama_decode(ctx_tgt)` →
  `common_speculative_process` (mirror nextn embd target→draft) → sample + verify
  jak ngram. Kluczowe rollbacki KV ctx_dft: do `base_pos` po `draft()` (kasuje
  autoregresyjne pre-advancement draftera) oraz do nowej wolnej pozycji po
  akceptacji (lustrza rollback ctx_tgt) — bez nich M-RoPE ctx_dft odrzuca batch.
  Oba konteksty i uchwyt speculative żyją wyłącznie w wątku schedulera. Pomiar:
  MTP daje realne przyspieszenie tylko gdy GPU nie jest wysycone (pojedynczy /
  rzadki strumień ~1.8x na Qwen3.6-27B Q4_K_S, RTX 4090); przy 4 równoległych
  sekwencjach narzut draftu przewyższa zysk.
- `tentaflow-wrappers/examples/llama_engine_smoke.rs` testuje silnik:
  `--speculative off|ngram|mtp`, realny licznik tokenów (pole
  `StreamToken.generated_tokens` na tokenie finalnym), per-request tok/s oraz
  dowód anty-hangu przy `--slow-consumer` (porównanie 1. fali requestów).
  Scenariusze regresji anty-hang (CR-001): `--drop-mid` (konsument porzuca
  Receiver w połowie → slot zwolniony, `LlamaEngine::inflight()` wraca do 0),
  `--silent-consumer` (konsument żywy ale niemy → po `stream_stall_timeout`,
  w teście 3s, slot zwolniony z Error, inne sloty działają), `--queue-overflow`
  (12 req na 4 sloty → wszystkie kolejkują i kończą, inflight=0, brak wycieku).
- `inference/llamacpp.rs` (`LlamaCppEngine`) wpina `LlamaEngine` jako backend
  `InferenceEngine` w core. Silnik trzymany jest w `RwLock<Option<Arc<LlamaEngine>>>`;
  `generate`/`generate_stream` biorą KRÓTKI read-lock, klonują `Arc` i zwalniają lock
  PRZED `submit` — żaden lock nie jest trzymany podczas generacji (anty-hang). Most
  stream→tokio nie tworzy wątku-per-request: `LlamaEngine::submit_with_sink` przyjmuje
  `Box<dyn EngineSink>`, a scheduler woła `sink.try_send` wprost ze swojego wątku.
  Core dostarcza `StreamSink`/`CollectSink` owijające `tokio::mpsc::Sender`
  (`try_send` → `SinkStatus::{Delivered|Full|Closed}`), więc setki równoległych
  requestów dzielą jeden wątek-scheduler bez globalnego locka i bez kanału std mpsc
  per slot. `tentaflow-wrappers` pozostaje BEZ zależności od tokio (sink jest
  generyczny). Anty-hang per-slot zachowany: `Full` odkłada token do `pending`
  slotu, terminalny token też idzie przez `pending` (deferred finish — `release_slot`
  zwalnia slot dopiero po opróżnieniu ogona, zero blokującego `send`). CR-001:
  `EngineConfig.stream_stall_timeout` (z deploy `stream_stall_timeout_secs`,
  domyślnie 60s) wymusza deadline POSTĘPU dostarczania — slot z niepustym `pending`,
  który od progu nie zmniejszył `pending` (konsument „żywy ale niemy", nigdy nie
  czyta i nie rozłącza się), jest siłą zwalniany z `FinishReason::Error`, więc po
  wyczerpaniu `queue_capacity` silnik nie odmawia w nieskończoność (cichy hang
  admission). CR-003: rdzeniowy `StreamToken` niesie `finish_reason: Option<StopReason>`
  + `error: Option<String>` na finale; `StreamSink` przekłada `FinishReason::Error`
  silnika na `error` (+`warn!`), a `stream_tokens_to_chunks` mapuje realny
  finish_reason (`length`/`stop`/`error`) zamiast twardego "stop". CR-004:
  `generate` liczy `tokens_per_second` od pierwszego tokena (TTFT) i raportuje
  realne `prompt_tokens` (pole `StreamToken.prompt_tokens` z silnika, nie 0).
  Deploy params → `EngineConfig`: `n_seq_max` z `n_parallel`/`max_concurrency`
  (domyślnie 8), `ctx_per_seq`=ctx_size, `n_batch`=batch_size, reszta z load-configu;
  speculative z `speculative_method`/`num_speculative_tokens`/`size_ngram`/`size_mgram`
  → `SpeculativeMode` (silnik sam ustawia `n_rs_seq` i odrzuca MTP bez głowy nextn).
- Embeddingi: silnik jest generation-only, więc `LlamaCppEngine::embeddings` używa
  ODRĘBNEJ, leniwie tworzonej ścieżki `LlamaRuntime` (kontekst `embeddings=true`)
  obok silnika generacji — zero regresji. Pełna unifikacja (embeddingi w silniku)
  jest świadomie odłożona. `LlamaRuntime` jest TYLKO ścieżką embeddingów: w Fazie 4
  usunięto z niego martwą generację (`generate`/`generate_streaming`/`generate_inner`
  + ręczny speculative `draft_tokens`/`verify_draft`/`rollback_memory`/
  `ngram_simple_draft` oraz martwe typy `LlamaGenerateConfig`/`LlamaGenerateOutput`/
  `LlamaStopReason`). Zostają `load`/`metadata`/`embeddings`/`tokenize`/`context`
  oraz współdzielone z silnikiem `build_sampler_chain`/`is_eog_with_model`/
  `token_to_piece_with_model`/`check_stop_sequence`. `SpeculativeConfig` zostaje
  (parsuje deploy params w `inference/llamacpp.rs`). Integracyjny test core
  `tentaflow-core/tests/llamacpp_engine_e2e.rs` (`#[ignore]`, env
  `TENTAFLOW_LLAMA_TEST_MODEL`) ładuje GGUF i sprawdza generate/stream + unload.
- `tentaflow-wrappers/examples/whisper_smoke.rs` ładuje model whisper.cpp przez
  nasz wrapper i wykonuje krótką transkrypcję ciszy, żeby sprawdzić runtime bez
  uruchamiania całego API.
- `inference-whisper` jest ZAWSZE wbudowany na nie-Apple (Linux/Windows w
  `tentaflow/Cargo.toml`, Android w `tentaflow-mobile/core/Cargo.toml`) — engine
  STT `whisper` musi byc dostepny w runtime na tych platformach, to nie jest
  feature opt-in. Apple (macOS/iOS) NIE linkuje whisper.cpp — STT idzie przez
  MLX-whisper / natywny Swift. Wpiete per-target przez osobny blok
  `[target.'cfg(...)'.dependencies]`, zeby Apple bylo z niego wykluczone.
- Whisper.cpp i llama.cpp ZAWSZE wspolistnieja w binarce (kazdy `gpu-*` feature
  pociaga `inference-llamacpp`), a oba projekty wnosza WLASNY, ROZNYCH WERSJI
  ggml (llama master vs whisper `v1.8.3`) z identycznymi nazwami symboli `ggml_*`.
  Zeby to nie konczylo sie `SIGABRT` (mieszanie dwoch implementacji ggml), whisper
  jest linkowany jako IZOLOWANY dylib: `build-whisper-cpp.sh` linkuje whisper +
  jego ggml w jeden `libwhisper_tf.so` z version-scriptem `{ global: whisper_*;
  local: *; }` (ggml_* UKRYTE), `whisper-rs-sys/build.rs` linkuje ten dylib
  dynamicznie (a NIE statyczne ggml), a `tentaflow/build.rs` kopiuje go plasko
  obok binarki (`$ORIGIN`). Dzieki temu llama.cpp zostaje statycznie ze swoim
  ggml i nie ma kolizji — bez dawnego hacka `--allow-multiple-definition`.
  Izolacja jest zaimplementowana dla Linux i macOS w `build-whisper-cpp.sh`;
  na Windows izolowany DLL (eksport whisper_* przez `.def`) buduje `build-all.ps1`.
- **INWARIANT izolacji symboli (OBOWIĄZKOWY dla każdej vendorowanej biblioteki natywnej).**
  Każda samowystarczalna biblioteka natywna (whisper, zvec, …) MUSI eksportować WYŁĄCZNIE
  własne publiczne API (`whisper_*`, `zvec_*`) i CHOWAĆ całą zbundlowaną resztę
  (protobuf, abseil, RocksDB, Arrow, ggml). Inaczej dwie kopie tej samej biblioteki C++
  (np. protobuf w zvec ORAZ w warstwie ONNX binarki) interpozycjonują się przez dynamiczny
  linker na Linuxie i psują stertę przy static-init (`corrupted size vs prev_size`, crash
  przed `main`; macOS chroni two-level namespace, ale i tak izolujemy). Mechanizm:
  Linux `-Wl,--version-script` z `{ global: <name>_*; local: *; }`; macOS
  `-Wl,-exported_symbols_list` z `_<name>_*`; Windows `.def`. zvec: `scripts/build-zvec.sh`.
  PUŁAPKA: deploy uruchamia binarkę z `LD_LIBRARY_PATH=target/<profile>`, który ma
  pierwszeństwo nad rpath do vendora — `tentaflow/build.rs` MUSI kopiować świeży vendored
  `.so/.dylib` do `target/<profile>` (pełna kopia, nie hardlink), inaczej stara kopia
  shadow'uje nowy vendored. Weryfikacja: `nm -D --defined-only <so> | grep -ic protobuf` = 0.
- Deploy `llama.cpp` z HuggingFace GGUF musi wskazywać pojedynczy plik `.gguf`
  (`config_json.model_file`) albo preset z `quantization`; downloader nie powinien
  pobierać wszystkich kwantyzacji z repozytorium.

## tentaflow-infer (FORGE)

Independent inference-engine project (own Cargo workspace, NOT part of the
main binary): Rust systems layer + **Mojo GPU kernels** (AOT → PTX + manifest,
zero Mojo runtime in the server; ADR-0001). Spec: `tentaflow-infer/docs/SPEC.md`,
plan: `docs/PLAN.md`, Mojo 1.0b API quirks: `kernels/mojo/MOJO_NOTES.md`.
Crates: forge-types/hal (CUDA via cudarc, VRAM arenas, CUDA graphs) /
formats (GGUF+safetensors+NVFP4, CPU golden dequant) / tokenize / kernels
(PTX registry + typed launchers, golden GPU tests) / engine (paged KV,
forward pass, scheduler queue) / server+cli (OpenAI API). Kernel toolchain:
`cd tentaflow-infer/kernels/mojo && pixi run mojo build_kernels.mojo`
(pixi env, gitignored; artifacts in `build/<arch>/` are committed).
E2E proven: Bielik-PL-Minitron-7B-NVFP4 (software FP4 dequant) generates
coherent Polish on the RTX 4090.

### Ścieżka NVFP4/FP8

- `FORGE_GEMM=fp8mod-ffn` włącza hybrydowy prefill: kernele Mojo przepakowują
  projekcje Q/O/gate/up/down NVFP4 oraz pojedynczy `lm_head` F16 do FP8 na GPU.
  Źródłowe NVFP4 pozostaje rezydentne dla decode; K/V nie są konwertowane.
- Wyspecjalizowane kernele małych batchy NVFP4 obsługują B4/B8/B16 i BM32.
  Decode GQA 4:1 dla `head_dim=128`, KV F16 i bez Q/K norm współdzieli K/V między
  czterema głowicami Q oraz używa dwugłowicowego `combine2`.
- Konwersję wolno rozpocząć dopiero po sprawdzeniu możliwości urządzenia,
  obsługiwanych kształtów, kompletu artefaktów i dostępnego VRAM. Niespełnienie
  warunków pozostawia całą warstwę na istniejącej ścieżce NVFP4.
- Zweryfikowany wynik RTX 4090, pp4096/jeden strumień: FORGE 10 302,7 tok/s
  prefill i 143,100 tok/s decode; vLLM 0.25.1 odpowiednio 9 732,9 i 146,372.
  Prefill wygrywa o 5,85%, decode pozostaje 2,24% wolniejszy. Protokół i
  ograniczenia są w `tentaflow-infer/docs/BENCH_NVFP4_VLLM.md`.
- HAL FORGE nadal obsługuje tylko CUDA. AMD/ROCm, Metal i natywne instrukcje FP4
  Blackwell nie są zaimplementowane; fallback możliwości nie oznacza obsługi
  tych backendów. Użyty checkpoint compressed-tensors NVFP4 nie jest bezpośrednio
  obsługiwany przez badaną wersję `llama.cpp`.

### Dekodowanie spekulatywne

- `forge-engine::speculation` ma wspólny kontrakt `Proposer`, typowane
  `DraftTree`/`DraftNode`, `SpeculationCoordinator`, kompozycję kaskadową i
  statystyki akceptacji per proposer. Węzły przenoszą źródło,
  `proposal_logprob` i `conditional_confidence`, dzięki czemu ten sam kontrakt
  obsłuży później greedy oraz lossless stochastic acceptance.
- Wykonawczo działa hostowy `NgramProposer`, natywne MTP/NextN oraz ich router
  priorytetowy dla gęstego hybrydowego GGUF `qwen35`. Natywne MTP wydziela
  bloki `nextn_predict_layers` z trunku, ładuje ich NVFP4/Q8_0/F32 bez drugiej
  kopii targetu i wykonuje proposer, weryfikację całego draftu oraz checkpointy
  DeltaNet/KV na GPU przez kernele Mojo. Serwer obsługuje `--speculative mtp`,
  `mtp:2`, `mtp:3` oraz priorytetowy router `mtp+ngram:2|3`; pełny draft n-gram
  omija proposer MTP, stan MTP jest doganiany po zaakceptowanym prefiksie, a
  brak pełnego draftu uruchamia natywne MTP. Budżet samodzielnego `mtp` 3 jest
  adaptacyjnie przełączany między K=2 i K=3.
  Przy wyłączonej spekulacji loader pomija opcjonalne wagi i stan NextN.
- Natywne MTP jest obecnie greedy-exact; domyślne `max_active=1` ogranicza pulę,
  a jawne `max_active > 1` przechodzi atomowy startup preflight i używa seryjnie
  przeplatanego forwardu per sekwencja. Target DeltaNet
  oraz draft MTP mają izolowany stan per sekwencja pod wspólnym lease z generacją
  i eventem GPU; `SeqKv` draftów korzystają z jednej współdzielonej puli stron
  MTP. Preflight dwóch slotów oraz audyt GPU pure MTP i MTP+n-gram A/B przechodzą.
  Produkcyjny E2E admission/cancel/reuse przechodzi dla dwóch sekwencji. Verifier
  utrzymuje osobne grafy T=3/4 według stabilnego identyfikatora slotu; reuse lease
  zachowuje adresy buforów GPU. Niespekulacyjny target ma pion B2: mixery
  zachowują osobne sloty, a FFN i głowa logits używają batch GEMM. Native MTP
  nadal wykonuje lane seryjnie i wymaga verifiera `[B,T]` do wzrostu aggregate
  throughput. B2 wymaga rezydentnego KV i obsługiwanych formatów wag; tiering
  przechodzi na seryjny fallback przed mutacją KV. Ścieżkę sprawdzono wykonawczo
  wyłącznie na CUDA/RTX 4090
  z `protoLabsAI/ThinkingCap-Qwen3.6-27B-MTP-GGUF`. Źródła Mojo zachowują podział
  umożliwiający przyszły codegen AMDGPU/Metal, ale backendy AMD i Metal nie są
  jeszcze podłączone ani przetestowane. `draft-model`, `eagle`, `dflash` i
  `dspark` nadal zwracają `Unsupported`; weryfikacja drzewa, sampling, PARD i
  suffix nie są jeszcze zaimplementowane.
- Retained checkpointy stanu DeltaNet usuwają ponowny skan 48 warstw podczas
  zatwierdzania zaakceptowanego prefiksu. Wyspecjalizowana głowa Q8 B3/B4 oraz
  scalone przygotowanie DeltaNet zmniejszają liczbę kerneli i kopii D2D. Stała
  część verifiera T=3/T=4 działa jako trwały graf, a pozycję bazową attention
  odczytuje z bufora GPU.
  Fair prose RTX 4090: około 102,0 tok/s dla raw128 oraz 72,6 tok/s dla raw512
  w trybie MTP K=3; `mtp+ngram:3` osiąga odpowiednio 118,897 tok/s oraz
  76,688 tok/s. Referencja pure MTP bez n-gramu z
  llama.cpp na tym samym modelu i prose osiąga około
  111,079 i 87,652 tok/s. Wszystkie przebiegi FORGE zachowały identyczne ID
  tokenów względem sekwencyjnego greedy; luka pure MTP pozostaje otwarta. Szczegóły:
  `tentaflow-infer/docs/BENCH_QWEN35_MTP_NVFP4.md`.
- `forge-formats` udostępnia zamknięty parser `forge-speculation.json` dla
  neuralnych proposerów. Manifest opisuje target, fingerprinty, tensory, cechy,
  dtype/kwantyzację, sampling, kalibrację oraz osobne licencje kodu i wag.
  `SpeculationManifest::load` ogranicza artefakty do katalogu manifestu i
  weryfikuje SHA-256 każdego pliku; porównanie fingerprintu z aktywnym targetem
  nastąpi przy integracji neuralnego runtime.
- Krótka weryfikacja DeltaNet T=2-4 ma scalony kernel Mojo
  `deltanet_prepare_t{2,3,4}_f16`. W jednym launchu wykonuje przyczynowy splot,
  zapis checkpointów okna, podział QKV, L2/repeat Q/K oraz bramki `g` i `beta`;
  wejściowy stan okna pozostaje niemutowany. Artefakty PTX wymagają `sm_80+`.
- NVFP4 B3/B4 na NVIDIA z warpem 32 używa dwóch wierszy na CTA, współdzielonej
  LUT E2M1 i szerokich odczytów aktywacji. Osobne symbole NVIDIA są wybierane
  przez launcher, a dotychczasowe B3/B4 pozostają przenośnym fallbackiem.
- Krótki skan DeltaNet T3/T4 dla `d_state <= 128` dzieli kolumny stanu na kafle
  szerokości warpa. Dla kształtu Qwen `32 x 128` daje 2,60-2,63x w izolowanym
  profilu RTX 4090 i około 5,9% krótszy pełny cykl MTP, zachowując bitową
  zgodność wyjścia i checkpointów. T2 i większe stany używają starej ścieżki.
- Krótkie Q8_0 B3/B4 na NVIDIA z warpem 32 używają czterech wierszy na CTA i
  DP4A dla dokładnych iloczynów int8. Przenośny kernel ośmiu wierszy pozostaje
  fallbackiem; F16/F32 są bitowo zgodne, a ważony miks Qwen zyskuje około
  11,5-13% w izolowanym pomiarze.
- Jawny launcher NVFP4 Q8_1/DP4A ma batched, bitowo zgodne warianty T3/T4.
  Zachowują kolejność szeregowego GEMV i współdzielą dekod wag między tokenami;
  domyślny dispatch pozostaje F16 do czasu pełnego A/B long-parity.
- NVIDIA NVFP4 B1 z dwoma wierszami na CTA ma tę samą matematykę co B3/B4 i
  jest z nimi bitowo zgodny na wszystkich projekcjach ThinkingCap. W izolacji
  jest 1,8-3,2x szybszy od starego B1 F16. Prose128/prose512, repeat i natywne
  MTP zachowują parity po ujednoliceniu serial i verifier na tej matematyce.
- Opcjonalny `FORGE_MTP_DRAFT_HEAD=nvfp4` przepakowuje współdzielony head Q8_0
  do osobnej kopii GGUF NVFP4 wyłącznie dla propozycji MTP. Packer i F32 GEMV są
  kernelami Mojo; target verifier nadal używa oryginalnego Q8_0.

## Hybrydowy prefill i catch-up MTP

- Hybrydowy target wykonuje prefill w macierzowych chunkach na GPU. Dynamiczne
  kernele DeltaNet obsługują pełne chunki, a stan rekurencyjny jest zatwierdzany
  po każdym z nich bez sekwencyjnego powrotu przez CPU.
- Natywny proposer MTP ze współdzielonym embeddingiem targetu wykonuje catch-up
  całego zaakceptowanego prefiksu jednym batchem. Scalony norm/join i dokładna
  projekcja Q8 przygotowują wejście, po czym aktualizowane są tylko K/V i carry;
  dedykowany embedding MTP pozostaje na legalnej ścieżce sekwencyjnej.
- Tymczasowa zamiana buforów prefill/verifier zawsze odtwarza bufory i grafy
  decode także po błędzie. Profilowanie liczy osobne spany dla każdego
  wewnętrznego chunka, również gdy prompt przekracza zewnętrzny limit 1024.
- Hybrydowe modele z natywnym MTP rezerwują 1152 MiB puli aktywacji, aby pomieścić
  jednocześnie bufory prefill, verifiera, stany DeltaNet i batched catch-up.
- Target KV ma zwartą mapę `global_layer -> kv_layer` i alokuje slaby wyłącznie
  dla warstw `Attention`. Qwen3.6-27B z układem 48 DeltaNet + 16 attention
  zużywa 64 KiB F16 KV na token zamiast 256 KiB; osobny cache MTP zachowuje
  jednowarstwową mapę identity.

## Histogramowy sampling GPU

- Aktywne kary repetition, frequency i presence są nakładane jednym kernelem
  histogramowym, po którym działa istniejąca równoległa selekcja argmax lub top-k.
- Domyślna ścieżka greedy bez kar nie wykonuje dodatkowego uruchomienia kernela,
  alokacji ani synchronizacji.

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
