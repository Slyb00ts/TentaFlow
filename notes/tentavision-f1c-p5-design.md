# Design F1c-P5 — Flow invoke DAG executor

Stan: design zatwierdzony przez PM (Predict = pełny real service_call).
Decyzje domyślne (Q1-Q4): concurrency cap=10/addon, wait_ms ceiling=30000ms,
backpressure drop-oldest OK przy Source>Predict, `flow.invoke` global (nie per flow_id).

ASSUMPTION-1: `flow_invoke_v1` host fn jeszcze nie istnieje. Plan zakłada utworzenie modułu `host_functions/flow.rs` od zera.

ASSUMPTION-2: istniejący `flow_engine/` to LLM-chat flows (inny model). Operatory TentaVision (Source/Predict/...) trafiają do NOWEJ rodziny `flow_runtime/` aby nie mieszać semantyk. Reuse: `cancel_on_drop`.

## A) Architektura

```
addon WASM
   │ flow_invoke_v1(flow_id, input_toml)
   ▼
host_functions/flow.rs ── permission check (flow.invoke) ── audit
   │
   ▼
flow_runtime/registry.rs   (loaded at addon install, lifecycle.rs)
   │   map<(addon_id, flow_id)> → CompiledFlow (operators[], edges[], topo[])
   ▼
flow_runtime/scheduler.rs  ── per-invocation orchestrator
   │   - cycle check (na compile)
   │   - tokio::spawn per operator
   │   - bounded mpsc channel(100) per edge, drop-oldest
   │   - collect Sink outputs
   ▼
flow_runtime/operators/{source,predict,threshold,branch,aggregate,sink}.rs
   │   each: async fn run(in_rx, out_tx[], ctx) -> Result<OperatorMetrics>
   ▼
audit_log (per operator start/finish/error + backpressure_drop collapsed 1/60s/edge)
flow_invocations DB (status, started_at, finished_at)
```

## B) Schema `*.flow.json`

```json
{
  "schema_version": 1,
  "id": "tv-realtime-adr",
  "operators": [
    { "id": "src1", "type": "Source", "params": { "stream": "camera.main", "fps": 5 } },
    { "id": "pred", "type": "Predict", "params": { "alias": "tentavision-yolo", "method": "detect", "classes": ["truck"] } },
    { "id": "thr",  "type": "Threshold", "params": { "field": "confidence", "min": 0.7 } },
    { "id": "br",   "type": "Branch", "params": { "expr": "class == 'truck'" } },
    { "id": "agg",  "type": "Aggregate", "params": { "window_ms": 5000, "op": "count" } },
    { "id": "snk",  "type": "Sink", "params": { "kind": "event", "topic": "alarm.created" } }
  ],
  "edges": [
    { "from": "src1", "to": "pred" },
    { "from": "pred", "to": "thr" },
    { "from": "thr",  "to": "br" },
    { "from": "br",   "to": "agg", "port": "true" },
    { "from": "agg",  "to": "snk" }
  ],
  "is_long_running": true,
  "max_runtime_ms": 0
}
```

Parser w `lifecycle.rs:1512+`: na install czyta `path`, waliduje, cycle-check (DFS), zapisuje w `flow_runtime::registry`. Limity: max 64 operatorów/flow, `port` tylko dla Branch.

## C) ABI

### `flow_invoke_v1`
```
fn flow_invoke_v1(in_ptr, in_len, out_ptr, out_cap, out_len_ptr) -> i32
```
Input TOML: `flow_id`, `input_toml` (nested), `wait_ms` (0 = async, >0 = sync timeout, max 30000).
Output TOML: `invocation_id` (UUIDv7), `status` ("running"|"completed"|"failed"), `result` (opt), `error` (opt).
Permission: `flow.invoke`. Risk class: **B**.
Default-deny: brak permisji → AbiError::Permission + audit denied reason=`missing_permission`.

### `flow_status_v1`
```
fn flow_status_v1(in_ptr, in_len, out_ptr, out_cap, out_len_ptr) -> i32
```
Input: `invocation_id`. Output: `status`, `started_at`, `finished_at`, `operators_completed`, `error`, `result_toml`.
Permission: `flow.invoke` (filtr WHERE addon_id=self — addon widzi tylko własne).

## D) Operatory

| Operator | Input | Output | Błąd | Audit |
|---|---|---|---|---|
| **Source** | (generator) | `Frame{camera_id, ts, frame_ref}` lub TOML | fail flow przy start error | `flow.op.source` |
| **Predict** | record | record + `prediction` | `on_error`: fail/skip/emit_null | `flow.op.predict` |
| **Threshold** | record z `field` | pass/drop | brak pola → drop (audit `field_missing`) | `flow.op.threshold` |
| **Branch** | record + `expr` | port `true`/`false`/`error` | expr error → port error lub fail | `flow.op.branch` |
| **Aggregate** | stream | windowed batch | overflow → emit+reset | `flow.op.aggregate` |
| **Sink** | record | side-effect (event_publish/sql_exec/ui_notify) | audit error, kontynuuj | `flow.op.sink` |

**Predict + alias**: używa `aliases::resolve` + `service::service_request` (rate-limited per addon, F1b P5). `is_active=0` → `alias_inactive`.

**Backpressure**: każda krawędź = `tokio::sync::mpsc::channel(100)`. Drop-oldest przez `try_send`; przy `Full` pop_front+push_back. Audit collapsed 1 wpis/60s/(flow_id,edge) z `dropped_count` (wzorzec `service_call_rate_limit`).

**Error semantics**: każdy operator ma param `on_error: "fail"|"skip"|"emit_null"` (default `fail`). Branch ma osobny port `error`. Sink zawsze `on_error=skip`.

**Per-operator timeout**: param `timeout_ms` (default 10000), `tokio::time::timeout`.

## E) DB migracja v29

```sql
CREATE TABLE flow_invocations (
    id TEXT PRIMARY KEY,                  -- UUIDv7
    addon_id TEXT NOT NULL,
    flow_id TEXT NOT NULL,
    started_at TEXT NOT NULL,             -- RFC3339 UTC
    finished_at TEXT,                     -- NULL while running
    status TEXT NOT NULL,                 -- 'running'|'completed'|'failed'|'cancelled'
    error TEXT,
    result_toml TEXT,                     -- only when completed, <64KiB
    operators_completed INTEGER NOT NULL DEFAULT 0,
    operators_total INTEGER NOT NULL
);
CREATE INDEX idx_flow_invocations_addon ON flow_invocations(addon_id, started_at DESC);
CREATE INDEX idx_flow_invocations_running ON flow_invocations(status) WHERE status='running';
```

Boot-time: `UPDATE flow_invocations SET status='failed', error='core_restart' WHERE status='running'`.

## F) Decyzje PM (zamknięte)

1. **Concurrency cap per addon**: **10** (audit `denied` reason=`max_concurrent_invocations`).
2. **wait_ms ceiling**: **30000ms** (limit sync wasmtime hostcall). Powyżej → force async.
3. **Source>Predict backpressure**: OK, drop-oldest to design.
4. **flow.invoke granular**: NIE, global. P6 może dodać.
5. **Predict**: pełny real service_call (NIE mock — sprzeczne z 'no stubs').

## Plan implementacyjny

| Chunk | Zakres | LOC | Zależy |
|---|---|---|---|
| **A** | `flow_runtime/types.rs` + `registry.rs` + parser flow.json (rozszerzenie `lifecycle.rs:1512`) + cycle DFS + migracja v29 | ~450 | — |
| **B** | `flow_runtime/scheduler.rs` (tokio orchestration + bounded channels + drop-oldest + collapsed audit + concurrency cap) | ~400 | A |
| **C** | `flow_runtime/operators/*.rs` (6 operatorów) | ~700 | B |
| **D** | `host_functions/flow.rs` (flow_invoke_v1 + flow_status_v1) + rejestracja + SDK wrappers | ~350 | C |
| **E** | Integration test: realny addon + `tv-realtime-adr.flow.json`, 30s stream → events, audit verify | ~250 | D |

**Total: ~2150 LOC.** Po każdym chunku: `cargo check` + codex review.

## Risk register

| Ryzyko | P | Impact | Mitygacja |
|---|---|---|---|
| Cycle w DAG ucieka detektorowi | Niskie | Wysoki | DFS przy parse (chunk A) + test |
| Memory leak long-running (Arc frame in queue) | Średnie | Wysoki | frame_ref (NIE bajty), soak 24h, dhat-rs |
| Tokio task leak na cancel mid-flight | Średnie | Średni | CancellationToken per invocation, unit test |
| Audit DoS 1000s drop/s | Wysokie | Średni | Collapsed 1/60s/edge |
| Predict timeout blokuje flow | Wysokie | Średni | Per-op `timeout_ms` 10s default |
| Concurrent invocation flood | Średnie | Wysoki | Cap 10/addon |
| Restart porzuca running | Pewne | Niski | Boot-time UPDATE → failed |
| Schema drift v1→v2 | Niskie | Średni | `schema_version` field |
