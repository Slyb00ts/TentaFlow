# Routing / Flow / Runtime Refactor Plan

Repo: `/Users/critix/repos/rust/TentaFlow`

Cel: uporzadkowac routing, flow engine i model/service resolution tak, zeby UI,
OpenAI API, flow builder i mesh uzywaly jednego zrodla prawdy.

## 1. Docelowa Zasada

TentaFlow ma miec jeden publiczny katalog modeli widoczny dla:

- Chat GUI model dropdown
- Flow Builder model selectors
- OpenAI-compatible `/v1/models`
- OpenAI-compatible request field `model`
- internal calls
- mesh/reverse calls

Ten katalog nie jest bezposrednio DB. Jest in-memory runtime catalog budowany z:

- uruchomionych service models ze wszystkich nodow
- published flows exposed as models
- aliases
- ACL/user visibility

Dla usera wszystko jest "modelem". Pod spodem wpis moze byc modelem z serwisu,
aliasem albo flow.

## 2. Public Model Catalog

Catalog entry kinds:

- `service_model`: concrete model exposed by a running service on any node
- `flow_model`: flow published as a model
- `alias`: alias resolving to service model, flow model, or another alias

Invariants:

- public model ids are unique after ACL filtering
- deploy/save must reject name collisions between service model, flow model, and alias
- GUI and `/v1/models` return the same visible model set
- all GUI dropdowns use this catalog, not raw DB tables

OpenAI API behavior:

```text
POST /v1/chat/completions { model: "x" }

x -> alias         -> recursively resolve
x -> service_model -> direct runtime call
x -> flow_model    -> execute flow
```

Flow publishing:

- Flow can be marked as published/as-model.
- Published flow has stable public `model_name`.
- Published flow enters unified model catalog.
- Permissions later filter this same catalog.

## 3. Execution Modes

There are two execution modes.

### Direct Model Call

Used when resolved target is `service_model`.

Examples:

- embeddings input -> embeddings output
- STT audio -> text / verbose JSON / diarization / speaker info
- TTS text -> audio
- LLM messages -> completion
- vision/omni LLM messages with multimodal parts -> completion

Direct calls still use aliases, fallbacks, strategy, mesh, embedded, HTTP, QUIC,
etc.

### Flow Execution

Used when resolved target is `flow_model`.

Flow executes nodes in order. Service nodes inside flow call the same runtime
executor as direct mode.

No flow adapter may implement its own QUIC/HTTP/embedded/mesh routing logic.

## 4. Alias Semantics

Existing alias fields:

- `alias`
- `target_model`
- `fallback_targets`
- `strategy`

Meaning:

- `target_model` is primary target.
- `fallback_targets` is ordered fallback list.
- each target can be:
  - concrete service model
  - published flow model
  - another alias
- `strategy` applies when a resolved concrete model has multiple available
  service instances.
- examples: `first_available`, `round_robin`, `least_loaded`

Resolution algorithm:

1. Resolve requested public model id.
2. If alias, resolve `target_model`.
3. If target has no available/successful backend before response starts, try
   `fallback_targets` in order.
4. Fallback target may be another alias, service model, or flow model.
5. Detect cycles and return clear error.
6. For concrete service model, collect all service instances from in-memory
   registry.
7. Apply alias `strategy` across instances.
8. Try candidates in strategy order.
9. For streaming, after first emitted token/frame, do not silently fallback.

Do not add `prefer_mesh`, `prefer_local`, `mesh_only`, `local_only` now.
Mesh/local is transparent and decided by service availability plus alias
strategy.

## 5. Remove `flow_engine_enabled`

The flag is no longer needed.

Actions:

- remove setting from seed
- remove `FlowDispatcher::is_enabled`
- remove runtime checks for `flow_engine_enabled`
- do not maintain legacy dispatch switch
- update docs/comments

There will be no "old routing mode vs flow mode" feature flag.

## 6. Fix `service_type`

Use endpoint/service categories:

- `chat`
- `stt`
- `tts`
- `embeddings`
- `documents`
- `agents`

Fix default seed:

- default LLM chat flow must be `service_type = "chat"`, not `"llm"`
- TTS flow should use `tts`
- STT flow should use `stt`
- teams/agent flow should use `agents` or a published `flow_model`

Important: migrate `flows.service_type`, not `flow_model_bindings.service_type`,
because binding table does not have service_type.

## 7. Fix ACL Double Dispatch

Current problem: user path can fallback into internal path that re-runs flow
without user context.

Required shape:

- one execution entry point carries `Option<UserContext>`
- public user calls always pass user context
- internal calls are explicit internal calls
- no `_for_user` method may fallback into a method that retries flow without
  user context

ACL applies to:

- visible model catalog
- direct service model execution
- flow model execution
- aliases resolving to either

## 8. ModelRuntimeExecutor

Create one executor, likely under `services/runtime/`.

It is the only place that knows how to execute resolved service models.

Responsibilities:

- resolve public model id through catalog/alias resolver
- handle alias fallbacks
- apply strategy across service instances
- execute local embedded models
- execute HTTP backends
- execute QUIC sidecars
- execute remote mesh forwarding
- handle direct STT/TTS/embeddings/chat
- preserve multimodal message parts
- enforce mesh hop limit
- produce route/runtime metadata

Use existing `services/handles_cache::BackendHandle` if possible. Do not create
a third parallel backend handle system.

Suggested API:

```rust
pub struct ModelRuntimeExecutor { ... }

impl ModelRuntimeExecutor {
    pub async fn execute_chat(&self, req: ChatCompletionRequest, ctx: ExecutionContext)
        -> Result<ChatCompletionResponse>;

    pub async fn stream_chat(&self, req: ChatCompletionRequest, ctx: ExecutionContext)
        -> Result<ChatChunkStream>;

    pub async fn execute_stt(&self, req: TranscriptionRequest, ctx: ExecutionContext)
        -> Result<TranscriptionResponse>;

    pub async fn execute_tts(&self, req: TTSRequest, ctx: ExecutionContext)
        -> Result<TTSResponse>;

    pub async fn execute_embeddings(&self, req: EmbeddingsRequest, ctx: ExecutionContext)
        -> Result<EmbeddingsResponse>;
}
```

## 9. Flow Adapters Must Use RuntimeExecutor

Refactor these adapters:

- `llm`
- `stt`
- `tts`
- `embeddings`

After refactor:

- adapter builds request from node config/context
- adapter calls `ModelRuntimeExecutor`
- adapter does not manually choose QUIC/HTTP
- adapter automatically supports embedded, mesh, HTTP, QUIC, local, remote

Fix `llm` multimodal regression:

- preserve `MessageContent::Parts`
- support vision/omni models
- do not collapse non-text content into empty strings

## 10. STT Options

Direct STT must support configurable output.

STT request/options should allow:

- plain text
- JSON / verbose JSON
- timestamps
- diarization
- speaker identification
- speaker confidence
- known/unknown speaker labels

Speaker identification and diarization models are system-managed and attached
to every STT path.

Flow node config should support:

```json
{
  "model": "",
  "language": "pl",
  "response_format": "text",
  "timestamps": false,
  "diarization": false,
  "speaker_identification": false
}
```

Possible STT output fields:

- `text`
- `segments`
- `speakers`
- `speaker_id`
- `speaker_name`
- `speaker_confidence`

Do not keep STT+speaker as chat-only hardcoded preprocessing.

## 11. Flow Engine Fixes

Required:

- streaming flow execution must create/update `flow_executions`, same as blocking
  flow
- validation and executor must agree on supported node types
- every node type accepted by executor must have registry metadata
- no hidden executor-only node types

Long-term:

- streaming transformer nodes, e.g. `pii_filter` and `tts_buffer`
- for now, direct streaming may keep response middleware outside flow until
  stream-transform adapters exist

## 12. Default Flows

Default flows should represent orchestration, not every simple direct call.

Seed examples:

- `Default Chat`: `trigger -> llm -> output`
- `Safe Chat`: `trigger -> llm -> pii_filter -> output`

Direct embeddings/STT/TTS do not need DB flow unless user explicitly builds
orchestration.

## 13. Handler Cutover

Handlers should resolve public model id through unified catalog.

Chat handler:

```text
parse request
get user context
resolve model id from catalog
if target is service_model -> runtime.execute_chat / stream_chat
if target is flow_model -> flow executor
if target is alias -> resolver decides final target
```

Same idea for:

- `/v1/models`
- `/v1/chat/completions`
- `/v1/embeddings`
- `/v1/audio/transcriptions`
- `/v1/audio/speech`
- document ingest APIs

## 14. Move/Delete Routing Code

After runtime executor migration:

Move infrastructure:

- HTTP backend client -> `services/backend`
- circuit breaker -> `services/backend` or `services/runtime`
- live runtime helpers -> `services/runtime`
- local inference handler -> `inference`
- local STT handler -> `stt`
- reverse mesh request handling -> `mesh`
- ACL user context -> auth/API layer
- stream helpers -> `services/runtime` or `services/stream`

Delete dispatch shells only after `rg` proves no callers remain:

- old chat dispatch
- old streaming dispatch
- old embeddings dispatch
- old tts/stt dispatch
- old middleware route resolver

## 15. Required Tests

Add tests for:

- catalog includes service models
- catalog includes published flow models
- catalog includes aliases
- alias resolves to service model
- alias resolves to flow model
- alias fallback order
- fallback target can be alias
- alias cycle detection
- strategy over multiple instances of same model
- direct embedded LLM call
- direct mesh LLM call
- flow LLM node uses same alias resolution as direct chat
- multimodal/vision message parts preserved
- STT plain text
- STT verbose JSON
- STT diarization
- STT speaker identification
- streaming no fallback after first emitted chunk
- ACL preserved in direct and flow execution
- `/v1/models` matches GUI-visible model catalog after ACL filtering
