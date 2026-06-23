# Token Usage Metrics + Quotas + Lease Coordinator — implementation spec

Single source of truth for the token-metrics feature. Per-model / per-user / per-group token
accounting, aggregated mesh-wide, with hard quota enforcement and a dynamically-elected
(HRW "collapse") lease coordinator. Tables live in the main `tentaflow.db` and ride the existing
Sync Ledger (NOT a separate SQLite file — reuse `core_registry` capture/materialize). All ids are
TEXT UUID / deterministic synthetic keys (never autoincrement INTEGER — those collide across nodes).

## Data model (3 new synced tables, migration #86)

### `token_usage_daily` — per-node daily counters (summed mesh-wide = global usage)
Single-writer-per-row (only the owning node mutates its rows; remote nodes only materialize).
```
id              TEXT PRIMARY KEY      -- deterministic: "usage:{node_id}:{org_id}:{user_id}:{model_id}:{usage_day}"
node_id         TEXT NOT NULL         -- accumulating node (in the key → no cross-node collision)
org_id          TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE
user_id         TEXT NOT NULL         -- sentinel "__system__" when no UserContext
model_id        TEXT NOT NULL         -- request.model (stable alias = model_registry.model_name)
usage_day       TEXT NOT NULL         -- "YYYY-MM-DD" (UTC). monthly = SUM over days where substr(usage_day,1,7)=?
prompt_tokens     INTEGER NOT NULL DEFAULT 0
completion_tokens INTEGER NOT NULL DEFAULT 0
total_tokens      INTEGER NOT NULL DEFAULT 0
request_count     INTEGER NOT NULL DEFAULT 0
updated_at      TEXT NOT NULL DEFAULT (datetime('now'))   -- node-local watermark, NOT synced
```
Indexes: `(org_id, user_id, usage_day)`, `(org_id, model_id, usage_day)`, `(updated_at)` (flusher).
Synced fields: everything except `updated_at`. LWW-tracked (later HLC op = larger cumulative value → monotonic).

### `token_quota` — quota limits (admin-edited, LWW-tracked, org-scoped)
```
id              TEXT PRIMARY KEY      -- uuid v4
org_id          TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE
scope_type      TEXT NOT NULL         -- 'user' | 'group' | 'model' | 'org'
subject_id      TEXT                  -- user_id / group_id / model_id ; NULL for scope_type='org'
model_id        TEXT                  -- optional model restriction (NULL = all models)
period          TEXT NOT NULL         -- 'daily' | 'monthly'
max_total_tokens INTEGER NOT NULL     -- the cap (counts total_tokens)
is_active       INTEGER NOT NULL DEFAULT 1
created_at      TEXT NOT NULL DEFAULT (datetime('now'))
UNIQUE(org_id, scope_type, subject_id, model_id, period)
```
Synced fields: all except created_at. LWW-tracked.

### `token_lease` — per-(quota,node,period) lease slices (coordinator-written, LWW-tracked)
```
id              TEXT PRIMARY KEY      -- deterministic: "lease:{quota_id}:{node_id}:{period_key}"
org_id          TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE
quota_id        TEXT NOT NULL         -- references token_quota.id (logical)
node_id         TEXT NOT NULL         -- lease holder
period_key      TEXT NOT NULL         -- "YYYY-MM-DD" (daily) or "YYYY-MM" (monthly)
base_used       INTEGER NOT NULL      -- node's OWN cumulative used (this period) at grant time
granted_tokens  INTEGER NOT NULL      -- additional tokens this node may spend → allowance = base_used + granted
coordinator_node_id TEXT NOT NULL
expires_at      TEXT NOT NULL         -- RFC3339; node honors last lease if coordinator offline
created_at      TEXT NOT NULL DEFAULT (datetime('now'))
UNIQUE(org_id, quota_id, node_id, period_key)
```
Synced fields: all except created_at. LWW-tracked.

## Sync wiring (per the core_registry recipe)
- `CoreSyncResourceKind`: add `TokenUsageDaily`, `TokenQuota`, `TokenLease`.
- `CORE_SYNC_DESCRIPTORS`: 3 entries, `scope = Organization`, `retention = Durable`, `partition_suffix = "tokens"`.
  resource_type: `core.token_usage_daily` / `core.token_quota` / `core.token_lease`.
- `core_materializer.rs`: 3 `apply_*` upsert fns (ON CONFLICT(id) full-row replace, preserve created_at), match arms, add all 3 to `is_lww_tracked`.
- `ensure_default_core_sync_policies`: automatic (iterates descriptors).
- repository.rs: `*_changed_fields` builders + capture calls (see below).

## Write paths (repository.rs)
- `bump_token_usage(pool, node_id, org_id, user_id, model_id, day, prompt, completion)` — local UPSERT
  (`total += prompt+completion`, `request_count += 1`, `updated_at = now`). **NO capture here** (hot path).
- `flush_token_usage_captures(pool, since_watermark) -> new_watermark` — selects rows with `updated_at > since`,
  emits `record_core_capture_for_org_tx(TokenUsageDaily, …, Update, fields)` per row. Called by the flusher (~60s).
- `token_quota` CRUD (create/update/delete) — capture on each.
- `token_lease` upsert — capture on each (coordinator only).
- Read helpers:
  - `global_usage_for_quota(pool, quota) -> i64` (SUM total_tokens across ALL node rows matching subject+period).
  - `node_usage_for_quota(pool, node_id, quota) -> i64` (this node's own rows only).
  - `list_quotas(org)`, `applicable_quotas(pool, org, user_id, model_id, group_ids)`.
  - usage aggregation queries for GUI (by user / by model / by group / by day).

## Enforcement (compliance/ai_gateway.rs)
`AiEventHandle` gains fields `org_id: String`, `user_id: String` (sentinel if none), `model_id: String`.
- **start_chat_event** (after `resolve_org_id`, before `start_ai_event`): call
  `enforce_token_quota(&conn, node_id, org_id, user_id, model_id)`:
  1. resolve user's group_ids; gather applicable active quotas.
  2. for each quota: if a fresh (non-expired) `token_lease` for (quota, this node, period) exists →
     reject when `node_usage_for_quota >= base_used + granted_tokens` (lease exhausted).
     else (no lease / coordinator cold) → reject when `global_usage_for_quota >= max_total_tokens`.
  3. reject = `Err(anyhow!(CoreError::RateLimitExceeded { message }))`.
- **finish_success** / **finish_stream_success**: call `bump_token_usage(...)` with the handle's
  org/user/model and the real `Usage { prompt_tokens, completion_tokens }` before `insert_ai_audit_row`.

## Background tasks (mesh/pipeline.rs, mirror spawn_trust_expiry_prune style)
- **spawn_token_usage_flusher** — every `token_flush_secs` (default 60): `flush_token_usage_captures`.
- **spawn_token_lease_coordinator** — every `token_lease_secs` (default 30):
  1. candidate set = self + trusted reachable nodes (sync_nodes ∩ peers connected/recently-seen).
  2. `elect_coordinator(org_id, candidates)` = HRW: `argmax over n of blake3("token-coord|{org}|{n}")`.
     (no epoch → stable while membership stable; set change → deterministic re-collapse).
  3. if elected node != self → do nothing (only coordinator writes leases).
  4. if self is coordinator: for each active quota & current period:
     `remaining = max(0, max_total - global_used)`; `per_node = max(MIN_LEASE, remaining / N)` capped so
     `sum(granted) <= remaining`; upsert `token_lease` for each active node with
     `base_used = that node's own current used`, `granted = per_node`, `expires_at = now + lease_ttl`.
  HRW lives in a pure fn `mesh/token_coordinator.rs::elect_coordinator(org, &[node_id]) -> Option<String>`
  (unit-tested: deterministic, all-nodes-agree, set-change re-collapse).

## Config (config/mod.rs)
New `[token_metrics]` (all optional, defaults baked):
`flush_secs=60`, `lease_secs=30`, `lease_ttl_secs=120`, `min_lease=1000`, `enabled=true`.

## Protocol (tentaflow-protocol + dispatch) — admin, binary CBOR (NOT REST)
New `MessageBody::TokenUsageBody(TokenUsagePayload)` (tentaflow-protocol/src/token_usage.rs).
Request/Response pairs:
- `UsageSummaryRequest { period, group_by (user|model|group|day), from, to }` / `…Response { rows }`.
- `ListQuotasRequest` / `…Response { quotas }`.
- `UpsertQuotaRequest { quota }` / `DeleteQuotaRequest { id }` / `…Ack`.
- `CoordinatorStatusRequest` / `…Response { coordinator_node_id, leases }`.
Handler in dispatch (`token_usage.rs`), `#[policy(UserSession)]` + permission gate.
Permission: read = `tokens.read`, write = `tokens.write`. Add to `role-org-admin` (write+read);
read also for `role-org-operator`/viewer per RBAC seed.

## GUI (www/js/modules/token-usage.js + app.js nav)
Admin module under `nav.section_management`. Tabs (tf-tabs):
- **Zużycie**: filters (okres daily/monthly, group_by), tf-table + tf-bar-chart/tf-line-chart of totals.
- **Limity**: tf-table of quotas + tf-modal editor (tf-select scope/period, tf-input max, tf-combobox subject, tf-toggle active).
- **Koordynator**: current coordinator node + live leases (tf-table). Read-only status.
All via existing tf-* components + ApiBinary over /ws/api. i18n keys pl+en.

## Build / verify order (commit per working phase)
A. tables+registry+materializer+repository builders/reads → `cd tentaflow-core && cargo build`.
B. ai_gateway enforce+bump + handle fields + config → core build.
C. pipeline flusher + coordinator + token_coordinator.rs (+ unit tests `cargo test token_coordinator`).
D. protocol body + dispatch handler + permissions → `cargo build` BOTH tentaflow-core AND the `tentaflow` binary
   (lesson: check both cargo graphs).
E. GUI module + nav + i18n.
Live verify on the rig mesh via Playwright GUI path once built.
```
