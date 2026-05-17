# TentaVision F1c — Handoff from F1b

**F1b Status:** Released v0.1.0-f1b (code-only acceptance; P2 lab pilot + P6
soak bug-bash deferred as F1c-opening gates)
**F1c Goal:** Addon UI signed iframe components + policy/claims engine +
vector storage + flow invoke + ONVIF one-click add, opened by closing the
deferred P2/P6 manual gates
**Estimated effort:** ~8-10 weeks for one senior, ~6 weeks for a 2-person team
**Related:**
- `notes/tentavision-plan.md` §15 (F1c row — Custom UI components) + §3.3
  (`[[ui_component]]` mapping) + §16.7 (Ed25519 + iframe sandbox decision)
- `notes/tentavision-f1b-acceptance-report.md` (carry-overs + deferred P2/P6)
- `RELEASE-F1b.md` "Known Limitations"

## Pre-conditions met by F1b

- Production camera vendors: RTSP (full ingest with exp-backoff reconnect +
  ±20% jitter) and ONVIF (WS-Discovery + real `test_connection`)
- Encrypted camera credentials (AES-256-GCM, 2-key rotation CLI archive +
  atomic rename; never plaintext in logs)
- Persistent HMAC keys for `pickup_token` / `frame_url` / `recording_url`
  with atomic staging → `.new` → live + crash recovery on startup
- Mesh HMAC key sync across trust-paired peers (`MESH_MSG_HMAC_KEYS_SYNC =
  0x44`); token minted on node A verifies on node B
- Cross-node frame pickup proxy (`MESH_MSG_FRAME_PROXY_REQUEST = 0x45` /
  `MESH_MSG_FRAME_PROXY_RESPONSE = 0x46`) with B-side replay protection
  (`mesh_inflight_consume`, `2 × TTL` window) and `frame_pickup_log.source_node_id`
- Audit Merkle hash chain (DB v25), `tentaflow-cli audit verify`, DoD-15
  closed
- `service_call` per-addon token bucket (1000 req/min default, burst 100;
  shared `util::token_bucket` primitive)
- TLS 1.3 only + HSTS always-on + optional mTLS pickup
- Per-IP + global HTTP rate limit, CORS whitelist, path-containment guard,
  audit `source_ip` capture
- ONVIF SSRF guard (probe URL path forced under `/onvif/`)

## F1c Scope

### 1. Custom UI components (Ed25519 + iframe sandbox) — primary

The F1a/F1b headline deferral. Per plan §3.3 + §16.7:

- New manifest section `[[ui_component]]` — `id`, `entry` (JS bundle path),
  `signature` (Ed25519 over bundle bytes), `mount_points` (M2 grid /
  M9 editor / M1 heatmap / M6 results), `host_permissions` (what postMessage
  bridge can request).
- Iframe sandbox: `sandbox="allow-scripts"`, no `allow-same-origin`. Bridge
  via `postMessage` with origin pinning. Frame URL = data: URL of bundle
  served from addon FS sandbox (no extra HTTP surface).
- Ed25519 verify on every install — reject bundle whose signature does not
  match the publisher key declared in `[manifest]`. The verify path reuses
  the existing `vendor/ed25519-dalek` already pulled in by transport / sidecar
  / protocol-wasm (no new dep).
- Four reference components (per plan §3.3 §3.1 row):
  - `tv-video-grid` — live camera grid with bbox overlay (M2)
  - `tv-zone-editor` — polygon editor on snapshot (M9)
  - `tv-heatmap` — occupancy heatmap (M1)
  - `tv-results-grid` — search results (M6)
- Host fn `ui_component_snapshot_v1` — addon-facing API for the editor to
  pull a frozen camera snapshot via the existing pickup token path
  (no new wire format).

### 2. Policy / claims engine (DPIA/FRIA gates)

Per plan §6.5 v1.2 schema + plan §10 D4 gating. Originally tagged F2; pulled
into F1c because D4 re-id production cannot ship without it and F2 scope is
already heavy with D1 ADR end-to-end.

- DB tables (already schema-drafted in §6.5): `policy_claims`,
  `policy_claim_signatures` (multi-sig DPO + supervisor), `legal_grants`,
  `gate_check_cache` (perf).
- Host fn `gate_check_v1(claim_kind, context)` — first-cut returns
  `Allow / Deny / RequireSignature`. Cached for the request lifetime.
- Audit row carries `related_claim_id` (already in F1a schema, NULL until
  F1c).
- CLI `tentaflow-cli policy issue --kind unmask --justification "..." --signer dpo`.

### 3. Vector storage full

F1a/F1b shipped a placeholder `vector_*_v1` ABI that returns empty results.
F1c wires real backend:

- Default backend: `usearch` (already vendored or via crates.io — to confirm
  in P1 of F1c). One on-disk file per namespace under
  `<tentaflow_home>/addons/<addon_id>/vectors/<namespace>.usearch`.
- Migration runner extension: `addon_vector_namespaces` table tracks
  `(addon_id, namespace, dim, metric)` for type-safety on insert.
- Rate-limit + size-limit per addon (reuse `util::token_bucket` from P5
  for write QPS).

### 4. Flow invoke + flow-templates

`flow_invoke_v1` is currently a NotImplemented stub. F1c wires DAG operators
from the manifest `[[flow_template]]` block:

- Operators (per plan §11): `Source`, `Predict`, `Threshold`, `Branch`,
  `Aggregate`, `Sink`. Single-process scheduler, no distribution (F2+).
- Backpressure: bounded channel between operators (capacity = 100, drop
  oldest with audit `result='backpressure_drop'`, same shape as streaming
  bus).
- D1 ADR flow as reference template (consumed by F2 anyway, so F1c can
  ship the engine + F2 wires the model bindings).

### 5. ONVIF GetStreamUri + one-click camera add

F1b shipped ONVIF discovery (`camera_discover_v1` returns
`Vec<DiscoveredCamera>` with vendor + model + XAddrs). F1c closes the UX:

- ONVIF `GetStreamUri` call (Media service) returns the RTSP URI for a
  given Profile. Wizard step "select discovered camera → enter
  credentials → confirm" calls `camera_add_v1` with vendor=`onvif`
  + derived `rtsp_uri`.
- M15 wizard step extension: "Discovered cameras" panel between step 3
  (Aliases) and confirmation. Reuses M14 readonly table pattern.

### 6. P2 lab cameras pilot (carry-over from F1b)

- Setup: 4 cameras across 2 vendors (Hikvision + Axis or similar).
- Integration test on real network: connect, stream 30 fps for 1 h,
  restart one camera mid-stream, verify reconnect (P1.B paths exercised).
- Bandwidth profiling: per-camera RTSP throughput (Mbps), decode CPU/GPU
  usage, appsink queue depth under sustained load.
- Acceptance: 4 cameras × 30 fps × 24 h with reconnect tolerance.

### 7. P6 soak gate (carry-over from F1b)

- Run `docs/SOAK_TEST.md` cumulative against F1a + F1b together.
- Memory leak deep-dive with `dhat-rs` if RSS growth > 5% / 24 h.
- FD leak investigation if `lsof` count grows monotonically.
- Bugs filed and fixed before F1c tag.

### 8. Multi-tenant RBAC (F2 candidate, pulled forward partial)

- User table + session table + permission grants. F1a/F1b admin is
  single-tenant (one root user implied by the OS process).
- Minimum F1c surface: per-user audit attribution (so DoD-9 / DoD-10
  events carry `actor_user_id` instead of `actor=system`).
- Full multi-tenant tenancy (org isolation, per-org addon installs)
  remains F2.

### 9. Carry-over deferred items from F1b

- Mesh broadcast-on-rotate (push new HMAC keys to all connected peers
  without waiting for next `PeerConnected`). Current lazy propagation is
  bounded by reconnect frequency + 2-key TTL window grace.
- Frame proxy chunked transport (>16 MiB / 4K RGB24). Mesh
  single-message cap is 16 MiB; current fallback is HD-and-below clean.
- Manifest `[runtime] rate_limit_per_min` per-addon override for the P5
  bucket.

## Design notes

### UI component sandbox

Iframe `src` is a `data:text/html;base64,...` payload containing the
addon's signed bundle. `sandbox="allow-scripts"` only — no
`allow-same-origin`, no `allow-top-navigation`, no `allow-popups`. The
bridge is `window.parent.postMessage({type, payload, request_id}, origin)`
where `origin` is the addon's pinned data URL. All host fn calls go
through the bridge — the iframe cannot fetch directly.

Ed25519 verify happens in `addon::install` against the publisher key from
the `[manifest]` block. A bundle whose signature does not verify causes
install to fail with `AbiError::InvalidArgument` and no rows in
`ui_components`. Sign-key rotation flow: addon must be re-installed
(no in-place key swap).

### Policy claims schema

Tables drafted in `notes/tentavision-plan.md` §6.5 v1.2:

```sql
CREATE TABLE policy_claims (
    id TEXT PRIMARY KEY,        -- ULID
    kind TEXT NOT NULL,         -- 'unmask' | 'retention_override' | ...
    subject TEXT NOT NULL,      -- 'camera:abc' | 'user:xyz'
    justification TEXT NOT NULL,
    issued_at TEXT NOT NULL,
    expires_at TEXT,
    revoked_at TEXT,
    revoked_by TEXT,
    revocation_reason TEXT
);

CREATE TABLE policy_claim_signatures (
    claim_id TEXT NOT NULL REFERENCES policy_claims(id),
    signer_role TEXT NOT NULL,  -- 'dpo' | 'supervisor'
    signer_id TEXT NOT NULL,
    signed_at TEXT NOT NULL,
    signature BLOB NOT NULL,
    PRIMARY KEY (claim_id, signer_role)
);
```

`gate_check_cache` table avoids re-evaluating policy for every
`service_call` in a request (cached for request_id lifetime).

### Vector backend choice

`usearch` (Apache 2.0, C++ core w/ Rust bindings, mmap on-disk, HNSW)
preferred over `qdrant` (process-per-node overhead) and `hnsw_rs`
(pure-Rust but no disk persistence). Confirm crate licence + benchmark
single-namespace 1 M vectors at F1c P1.

### Migration path F1b → F1c

DB migrations v26+ (additive — no breaking):
- v26: `ui_components`, `ui_component_grants` (component → addon grants).
- v27: `policy_claims`, `policy_claim_signatures`, `legal_grants`,
  `gate_check_cache`.
- v28: `addon_vector_namespaces`.
- v29: `users`, `sessions`, `user_permissions` (RBAC partial).

Manifest schema additions (non-breaking — all new optional sections):
- `[[ui_component]]` (id, entry, signature, mount_points, host_permissions)
- `[[flow_template]]` (operators, edges, parameters)
- `[runtime] rate_limit_per_min` (overrides P5 default 1000)
- `[publisher] ed25519_public_key` (sign verification for ui_component
  bundles)

## Estimated effort breakdown

| Task | Effort |
|------|--------|
| P2 lab pilot (carry-over from F1b) | 1 week |
| P6 soak bug-bash (carry-over from F1b) | 0.5-1 week |
| UI signed iframe components (4 reference) | 3 weeks |
| Policy/claims engine | 2 weeks |
| Vector storage full | 1 week |
| Flow invoke + DAG operators | 1.5 weeks |
| ONVIF GetStreamUri + wizard step | 0.5 week |
| Multi-tenant RBAC partial (audit attribution) | 0.5 week |
| **Total F1c** | **~8-10 weeks** (one senior) |

## Out of F1c scope

These remain in later phases (per `tentavision-plan.md` §15):

- D1 ADR Flow end-to-end + 4 inference blocks + services yolo/ocr in Docker
  — F2 (consumes flow-invoke + claims-engine shipped here)
- Evidence signing (HSM + TSA RFC 3161), full recording retention engine,
  real-time audit-tamper alert — F3
- D3 luggage Flow, M9 zone editor consumer (component shipped here), 4+
  cameras × 3 profiles — F3
- Re-id D4 production (uses claims engine shipped here, but model bindings
  are F7)
- PostgreSQL backend — F8
- Distributed (cross-mesh) rate limit for `service_call` — F2+
- Frame proxy >16 MiB chunked transport — F1c-stretch / F2 depending on
  4K demand from lab pilot
