# TentaVision F1b — Implementation Plan

**Status:** Phase 1 done (P1.A/B/C/D — RTSP/ONVIF camera vendors)
**Source:** `notes/tentavision-f1b-handoff.md` (F1a → F1b)
**Started:** 2026-05-16 (post v0.1.0-f1a tag)

## Roadmap (z handoff)

| Phase | Scope | Effort | Status |
|-------|-------|--------|--------|
| **P1** | RTSP/ONVIF cameras + credentials | 2 weeks | in progress |
| **P2** | Lab pilot 4 fizyczne kamery | 1 week | pending |
| **P3** | Multi-node mesh key sync | 2 weeks | pending |
| **P4** | Audit Merkle chain (DoD-15 full) | 1 week | done |
| **P5** | service_call rate limit | 0.5 week | done |
| **P6** | Bug bash z F1a soak run | open | pending |

## Phase 1 — RTSP/ONVIF camera vendors

Phase 1 jest dzielony na cztery Chunki tak, by każdy był osobnym
deployable commitem i mógł być review'owany niezależnie. Chunki B-D
wymagają fundamentów z A (extended vendor CHECK), więc kolejność jest
ustalona.

### P1.A — RTSP fundamenty (THIS CHUNK, in progress)

**Scope:**
- DB migration v23 — `cameras.vendor` CHECK rozszerzony o `rtsp` i `onvif`
  (SQLite table-rebuild pattern: create-new + insert-select + drop + rename,
  z `PRAGMA foreign_keys = OFF` / `ON` i `DROP TABLE IF EXISTS cameras_new`
  dla idempotencji częściowo nieudanego wcześniejszego runu).
- Indeksy v21 (`idx_cameras_camera_id_active` partial unique,
  `idx_cameras_owner`, `idx_cameras_status`) odtwarzane po rebuild.
- Cargo deps recon — bez zmian. `gstreamer = "0.23"` plus
  `gstreamer-app = "0.23"` (feature `camera`) są wystarczające; pluginy
  RTSP (`rtspsrc`, `rtph264depay`, `h264parse`, `avdec_h264`) są ładowane
  runtime'em z systemowego gst-plugins-good / gst-plugins-bad /
  gstreamer-libav. `aes-gcm = "0.10.3"` i `rand = "0.10.1"` już istnieją
  (potrzebne dla Chunka C, w A nieużywane).
- Plan w `notes/tentavision-f1b-implementation.md` (ten plik).

**Files touched:**
- `tentaflow-core/src/db/migrations.rs` — `CAMERAS_VENDOR_CHECK_RTSP_ONVIF`
  + wpis (23, "cameras_vendor_check_rtsp_onvif", ...) w `get_migrations()`.
- `tentaflow-core/tests/db_migrations_v23.rs` — nowy plik testowy
  (6 testów: preserves fake_file, allows rtsp, allows onvif, rejects
  unsupported, recreates indexes, recorded in `_migrations`).

**Acceptance:**
- `cargo test --features camera --test db_migrations_v23` — wszystkie testy
  przechodzą.
- Pozostałe testy migracji (`db_migrations_v8_v12`) nadal przechodzą
  (regresja sanity).
- Brak nowych entries w `Cargo.toml`.

**Out of scope w P1.A (przesunięte do B/C/D):**
- Kod connectora RTSP (`services/camera_ingest/rtsp.rs`) — Chunk B.
- Akceptacja `vendor="rtsp"` w `camera_add_v1` — Chunk B (wymaga gotowego
  connectora; teraz `SUPPORTED_VENDORS` w `addon/host_functions/camera.rs`
  pozostaje `["fake_file"]`).
- AES-GCM encrypt/decrypt `credentials_encrypted` + CLI key rotation —
  Chunk C.
- WS-Discovery `camera_discover_v1` + RTSP probing
  `camera_test_connection_v1` — Chunk D.

### P1.B — RTSP connector (done, commit e301071 + redact/test fix)

**Scope:**
- `tentaflow-core/src/services/camera_ingest/rtsp.rs` z pipeline:
  `rtspsrc location=... ! rtph264depay ! h264parse ! avdec_h264 !
   videoconvert ! video/x-raw,format=RGB ! appsink`.
- Reconnect — exponential backoff (1/2/4/8/max 60s) z ±20% jitter,
  reset przy osiągnięciu stanu FLOWING.
- Integracja z `CameraSession` + `Supervisor` (już zbudowane w F1a M1.W6
  pod `fake_file`).
- `SUPPORTED_VENDORS` w `addon/host_functions/camera.rs` rozszerzony
  o `"rtsp"`.
- System-level wymagania dla pakietów dystrybucji w `docs/SOAK_TEST.md`:
  Debian/Ubuntu — `gstreamer1.0-plugins-good gstreamer1.0-plugins-bad
  gstreamer1.0-libav gstreamer1.0-rtsp`; macOS — `brew install gstreamer`.

### P1.C — Credentials encryption (in progress)

**Scope:**
- AES-GCM encrypt/decrypt dla `cameras.credentials_encrypted` z kluczem
  z `<tentaflow_home>/keys/cameras.key` (256-bit, generowany przy pierwszym
  uruchomieniu, rotacja przez CLI).
- Real implementacja `camera_credentials_rotate_v1` (F1a noop).
- CLI: `tentaflow-cli camera rotate-key`.
- Audit row w `audit_log` z `risk_class='A'` przy każdej rotacji (kontekst:
  zmiana sekretów RTSP).

**Status:**
- `services::camera_ingest::credentials` — AES-256-GCM cipher (nonce 12B,
  tag 16B), `load_or_generate()` z atomic write + 0o600 na Unix. Env
  override `TENTAFLOW_CAMERAS_KEY`. Singleton `credentials_cipher()`.
- `camera_add_v1` przyjmuje optional `credentials_b64` (base64 z `user:pass`),
  walidacja długości + separator, encrypt z master key, store w
  `cameras.credentials_encrypted`.
- RTSP connector dekryptuje przed każdym `build_rtsp_pipeline`; helper
  `overlay_credentials(url, "user:pass")` odmawia overlay gdy URL już
  zawiera credentials.
- `camera_credentials_rotate_v1` — real: walidacja b64, encrypt z bieżącym
  master key, `set_camera_credentials_encrypted` (UPDATE z ownership guard),
  audit `result=ok` z `details=blob_len=X cleared=bool` (nigdy plaintext).
- CLI `tentaflow-cli camera rotate-key`: generuje nowy klucz, walk
  `list_all_camera_credentials_blobs`, re-encrypt każdy blob w transakcji
  (`replace_camera_credentials_blobs`), archiwum starego klucza jako
  `cameras.key.YYYYMMDD-HHMMSS`, atomic rename nowego klucza.
- DB: nowe helpery `set_camera_credentials_encrypted`,
  `list_all_camera_credentials_blobs`, `replace_camera_credentials_blobs`.
  `CameraRow` + `insert_camera` rozszerzone o `credentials_encrypted`.
- Testy: 13 unit (credentials.rs::tests) + 6 integration
  (tests/credentials_rotation.rs) — wszystkie zielone.

### P1.D — ONVIF discovery (done, commit 3acf1b8 + SSRF hardening)

`onvif_test_connection` forces the probe URL path under `/onvif/` so an
addon cannot use `camera_test_connection_v1` to HEAD arbitrary HTTP
targets on the local network. Unit test
`test_onvif_test_connection_forces_onvif_path` covers the three branches
(bare host, ONVIF sub-service preserved, arbitrary path rewritten).


**Scope:**
- WS-Discovery przez UDP multicast 239.255.255.250:3702.
- Probe SOAP envelope z typem `NetworkVideoTransmitter`; parse XAddrs
  z odpowiedzi; query Device service po manufacturer + model.
- Real implementacja `camera_discover_v1` (F1a zwraca pusty `Vec`).
- Real implementacja `camera_test_connection_v1` (F1a tylko fake_file)
  — RTSP DESCRIBE/SETUP round-trip z timeoutem.
- `SUPPORTED_VENDORS` rozszerzony o `"onvif"`.

## Acceptance per Phase 1 (cumulative)

- DB v23 migration idempotent (P1.A — done w tym chunku).
- RTSP `camera_add_v1` accepted (P1.B).
- RTSP session connects, streams frames, reconnects po disconnect (P1.B).
- Credentials zaszyfrowane w DB, nigdy plaintext w logach (P1.C).
- ONVIF discovery zwraca co najmniej 1 kamerę na sieci lab z jednym
  urządzeniem ONVIF (P1.D).

## Phase 3 — Persistent HMAC keys

### P3.A — Single-node key persistence (done)

Three HMAC signing keys used by `PickupTokenIssuer` (M1.W7),
`SignedUrlIssuer{FrameUrl}` and `SignedUrlIssuer{Recording}` (M1.W8) are now
persisted on disk under `<tentaflow_home>/keys/`:

- `pickup_token.key` (32 B, mode 0600) — 30 s one-shot tokens.
- `frame_url.key` (32 B, mode 0600) — frame signed URLs (max 10 min TTL).
- `recording_url.key` (32 B, mode 0600) — recording signed URLs (max 1 h TTL).

Each file is read on first issuer access (lazy `OnceLock` singletons in
`services::mod.rs`) and generated atomically with `getrandom::fill` + `tmp
→ rename + chmod 0600` if absent. Interrupted rotations are recovered on
startup: a `<name>.key.new` next to the live file is promoted (durable
commit marker), a `<name>.key.staging` is discarded (never committed).

Operator-driven rotation: `tentaflow-cli keys rotate <name>` (mirrors the
`camera rotate-key` staging → .new → live atomic flow). On a running host
the issuer keeps the previous key in memory as a verify-only secondary for
`max_ttl + 5 s` so any token minted seconds before the rotate still
verifies until its natural expiry. The operator must restart the host to
load the new key for signing.

Restart impact: pre-P3.A every restart invalidated all outstanding signed
URLs and pickup tokens (process-local `OsRng` key); post-P3.A the same
keys come back from disk so URLs minted before the restart remain valid
until their TTL expires.

### P3.B — Mesh sync (done)

Each peer now mirrors its three HMAC issuer keys (`pickup_token`,
`frame_url`, `recording_url`) to every trust-paired peer over the existing
mTLS-protected mesh stream. Effect: a `PickupToken` minted on node A
verifies when picked up at node B, and a `SignedUrl` signed on A verifies
on B, without sharing on-disk key files. Tokens stay scoped to the issuing
key + scope literal, so a frame URL still cannot replay as a recording URL.

Wire layer:

- New discriminant `MESH_MSG_HMAC_KEYS_SYNC = 0x44` carrying
  `HmacKeysSyncPayload { from_node_id, keys: [HmacKeyEntry] }`. Each
  `HmacKeyEntry` has `scope` (`"pickup_token"` / `"frame_url"` /
  `"recording_url"`), `current_key` (32 B), an optional `previous_key`
  (rotation grace), and a diagnostic 8-byte `key_id`.
- `IrohMeshManager::send_hmac_keys_sync` + `IrohMeshEvent::HmacKeysSyncReceived`
  mirror the existing `TrustedKeysSync` shape; the dispatcher in
  `mesh::pipeline` re-uses the same trust gate (sender must be `is_trusted`,
  otherwise the message is logged and dropped).

Receiver state:

- `services::mesh_keys::MeshKeyPool` — process-wide singleton holding
  `RwLock<HashMap<NodeId, PerPeerKeys>>`. Each scope's verify hot path
  (`PickupTokenIssuer::verify_only` / `consume_one_shot` /
  `SignedUrlIssuer::verify`) takes one read lock, collects the candidate
  key set, drops the lock, then runs constant-time HMAC compares for every
  candidate (no early-exit timing leak).
- Local keys verify first under the full inflight + one-shot contract; the
  mesh fallback runs only when the local-key path returns
  `InvalidSignature`. One-shot semantics for mesh-issued pickup tokens are
  owned by the issuing node — receiver-side replay protection is out of
  scope for F1b (the token still HMAC-verifies + has a 30 s expiry, so the
  attacker window is tight).

Lifecycle:

- `handle_peer_connected` (`mesh/pipeline.rs`) pushes a fresh advertise
  after the existing `TrustedKeysSync` block. It shares the same
  `is_trusted` gate as `TrustedKeysSync` but is intentionally **outside**
  the `last_sync_sent` cooldown — every trusted `PeerConnected` event
  re-advertises HMAC keys. Reason: HMAC keys can rotate via
  `tentaflow-cli keys rotate`, so a fresh advertise on every reconnect
  guarantees peers see the current key set without waiting for the 30 s
  cooldown to expire. The payload is small and `PeerConnected` is rare
  (post-pairing), so the extra traffic is negligible.
- `handle_peer_disconnected` drops every scope held for that peer; the
  next reconnect re-advertises. No on-disk persistence, by design — a
  revoked peer cannot leave stale verifiers behind.
- `TrustRevokedReceived` propagation also drops the revoked peer's
  entries (both the "I was revoked" branch and the "someone else was
  revoked" branch).

Rotation: a running `tentaflow-cli keys rotate <name>` triggers the file
watcher (`services::key_storage::watcher`) which calls
`rotate_in_memory` on the local issuer. The 2-key in-memory window keeps
tokens minted under the old key valid for `ttl + grace`; peers re-pick the
fresh key on their next `PeerConnected` advertise. An explicit
broadcast-on-rotate path (push new keys to all connected peers without
waiting for reconnect) is deferred — flagged in the README so operators
who rotate a hot key know to expect lazy propagation.

Tests:

- `services::mesh_keys` unit suite (8 tests) covers pool upsert, expired
  previous-window exclusion, peer drop, scope round-trip, and ingest
  validation (wrong length, unknown scope).
- `tests/mesh_key_sync_integration.rs` (4 tests) drives the full advertise
  → ingest → verify path: pickup token cross-node, signed URL cross-node
  for both scopes, rotation grace propagation, and a trust-boundary
  contract test that documents why the pool itself is trust-agnostic
  (the gate lives in `pipeline.rs`).

### P3.C — Cross-node frame pickup (done)

Closes the cross-node loop opened by P3.B. When node A signs a pickup token
for a frame whose bytes live in A's LRU but the service calling
`/core/frame/pickup` is connected to node B (mesh-fallback HMAC verify),
B fetches the bytes from A over the trust-paired mesh stream and serves
them to the service as if the pickup had been local.

Wire layer (P3.C-1, commit db226d3): new discriminants
`MESH_MSG_FRAME_PROXY_REQUEST = 0x45` and
`MESH_MSG_FRAME_PROXY_RESPONSE = 0x46`. Payloads
`FrameProxyRequestPayload { raw_ref, request_id }` and the three-variant
enum `FrameProxyResponsePayload { Found / NotFound / Unavailable }`. The
matching `IrohMeshEvent::FrameProxyRequestReceived` /
`FrameProxyResponseReceived` events are emitted by `handle_mesh_uni`
after the same pre-trust gate as every other 0x4* discriminant.

Verifier provenance + replay (P3.C-2, commit 200974d):
`PickupTokenIssuer::verify_only_with_source` returns
`(TokenPayload, VerifySource::{Local, Peer(node_id)})` in constant time
(always evaluates both candidate key sets). `mesh_inflight_consume` adds
B-side replay protection: the first cross-node consume for a given wire
records the timestamp, every subsequent one returns `AlreadyConsumed`
for `2 × token TTL`. DB v24 adds the nullable
`frame_pickup_log.source_node_id` column for audit.

HTTP integration (P3.C-3): `api::frame_pickup::verify_pickup_headers`
splits the header-verify step out of `handle_pickup` so the hyper handler
in `dashboard/server.rs` can dispatch by `VerifySource`. `Local` runs the
existing local-LRU one-shot consume; `Peer(node_id)` calls
`issuer.mesh_inflight_consume(wire)` first (replay guard), then
`frame_proxy::client::fetch_from_peer(iroh, peer, raw_ref, 5 s)` over
the mesh stream. Response mapping:

| Outcome | HTTP | `frame_pickup_log.result` | `source_node_id` |
|---|---|---|---|
| `Found{bytes,meta}` | 200 + body + width/height/pf/ts headers | `ok` | `Some(peer)` |
| `NotFound` | 404 | `frame_purged` | `Some(peer)` |
| `Unavailable{reason}` | 503 + `Retry-After: 5` | `upstream_unavailable` | `Some(peer)` |
| Timeout (5 s) | 503 + `Retry-After: 5` | `upstream_unavailable` | `Some(peer)` |
| Replay (B-side) | 403 | `replay` | `Some(peer)` |
| `mesh_unavailable` (no IrohMeshManager) | 503 + `Retry-After: 5` | `upstream_unavailable` | `Some(peer)` |

Hardening that stays unchanged: the 1 KiB GET body cap (P1.C-2), the
signed-URL rate limit (E1), and the universal security headers (E2) all
apply BEFORE the verify split, so cross-node and local pickups land on
the same rate-limit + body-size + header surface.

Tests:

- `tests/mesh_frame_proxy_dispatch.rs` (P3.C-1, 2 tests) — wire-level
  round-trip via two real `IrohMeshManager` instances.
- `tests/frame_pickup_cross_node.rs` (P3.C-3, 5 tests) — outcome → HTTP
  status / log_result mapping, plus the `mesh_inflight_consume` first-ok /
  second-replay contract.
- Existing `tests/streaming_pickup.rs` (12) + `streaming_pickup_e2e.rs`
  (6) continue to pass; the split keeps the local-path contract
  byte-for-byte identical.

## DB schema notes

Po P1.A schema cameras wygląda identycznie jak v21 z jedną zmianą:

```
vendor TEXT NOT NULL CHECK(vendor IN ('fake_file', 'rtsp', 'onvif'))
```

Wszystkie pozostałe kolumny, defaulty, CHECK'i innych pól oraz indeksy
są zreplikowane 1:1 z v21 (`CAMERAS_TABLE`).

## Phase 4 — Audit Merkle hash chain (DoD-15, done)

Closes the F1a deferral on DoD-15 (tamper-evident audit log). Every row in
`audit_log` now carries two BLOB columns:

- `prev_hash` (32 B) — the previous row's `hash`, or all-zero for the
  genesis row.
- `hash` (32 B) — `SHA256(canonical(row) || prev_hash)`.

Canonical row serialization is a `\0`-joined UTF-8 concatenation of every
DB-visible column in a fixed order (see
`tentaflow-core/src/audit/chain.rs::canonical_row_bytes`). Hash algorithm
is SHA-256, unsalted — anyone with DB read can verify, no secret material
to manage.

### Schema migration v25

`tentaflow-core/src/db/migrations.rs::audit_log_add_merkle_chain_columns`
(Rust step). Idempotent via `PRAGMA table_info(audit_log)` — skips the
ALTER when the column already exists, mirroring the v24 pattern.

Existing F1a / pre-P4 rows stay NULL in both columns. `verify_chain`
counts them as `legacy_unchained` so a post-upgrade verify does NOT flag
the entire history as tampered.

### Writer audit-paths updated

Every code path that writes to `audit_log` now computes the chain pair
under the same DB lock (so SELECT(latest hash) + INSERT is linearizable):

- `addon/host_functions/mod.rs::audit_log_with_risk` (primary host-fn
  writer; covers `audit_log` shim too).
- `audit/mod.rs::AuditLogger::flush` (batched buffer).
- `db/repository.rs`: `log_audit`, `log_audit_full`,
  `audit_alias_resolve_denied_within_tx`,
  `audit_reconcile_uses_alias_within_tx`,
  `audit_consumer_revoked_by_manifest_within_tx`.

All five writers now bind a pre-rendered `YYYY-MM-DD HH:MM:SS` timestamp
(was `datetime('now')`) so the same string lands in the DB and in the
hash input. Severity is always set explicitly (no longer relying on the
schema `DEFAULT 'info'`) so the verifier reads the same value the writer
hashed.

### Verifier

`tentaflow-core/src/audit/verify.rs::verify_chain(&Connection) ->
VerifyReport`. Walks every row in id order, recomputes the hash, and
classifies findings into `chained_ok`, `legacy_unchained`, and a vector
of `TamperedRow { id, kind }` with kinds:

- `PrevHashMismatch` — stored `prev_hash` does not match the previous
  row's `hash` (insert/delete in the middle of the chain).
- `HashMismatch` — stored `hash` is not `SHA256(canonical(row) ||
  prev_hash)` (row content modified after write).
- `NullHashAfterChainStart` — bypass writer slipped in a NULL-chain row
  after the chain had started.
- `MalformedHashBlob` — BLOB is non-NULL but not 32 bytes.

### CLI

`tentaflow-cli audit verify [--db-path PATH]`. Exit code:

- `0` — clean chain (no tamper).
- `1` — tamper detected (per-row reason printed to stdout).
- `2` — verification error (DB unreachable, etc).

### Tests

- `audit::chain::tests` (5 unit) — canonical serialization determinism,
  collision resistance via NUL separator, prev_hash chaining.
- `audit::verify::tests` (9 unit) — empty / genesis / 10-row chain /
  modified hash / modified content / inserted row / deleted row /
  legacy NULL rows / NULL after chain start.
- `tests/security_audit_chain.rs` (4 integration) — drives `log_audit`
  end-to-end and asserts every tamper scenario via the public verifier
  API.

### Performance

Each audit write costs one extra `SELECT hash FROM audit_log WHERE hash IS
NOT NULL ORDER BY id DESC LIMIT 1` plus one SHA-256 over ~200-500 B. At
audit-heavy workloads (camera ingest + service calls) the SELECT hits the
hot rowid index — measured sub-50 µs on a 100 k-row table in the
verify-tests bench, well below the existing rusqlite + WAL fsync floor.
No bench regression observed in the F1a soak harness; if a future
profiling pass shows it as a hot spot, the trivial mitigation is an
in-memory `OnceLock<Arc<Mutex<ChainHash>>>` cache of the latest hash
seeded on first write per process.

### Out of P4 scope

- Real-time tamper alerting (admin dashboard alert when verify detects a
  break) — out of F1b. Operators are expected to run
  `tentaflow-cli audit verify` from nightly cron.
- Per-row signature with the operator's private key — would harden against
  a DB-level attacker forging both content + hash. Deferred to F3
  (evidence signing with HSM + RFC 3161 TSA).

## Phase 5 — service_call rate limit (done)

Per-addon token-bucket limiter guarding `service_call_v1`
(`addon::host_functions::service::service_request`). Without it a single
buggy or malicious addon spamming 10 000 req/s drains shared backend
services (yolo, whisper, ...) — this is the first line of defence before
the alias resolver / QUIC dispatch.

### Design

- **One bucket per addon_id**, keyed in a `DashMap`. Bounded by an LRU
  eviction at `MAX_ADDON_ENTRIES = 10 000` plus an idle sweep
  (`IDLE_EVICT_AFTER = 600 s`). Same shape as the per-IP HMAC limiter in
  `api::rate_limit` — that one's `TokenBucket` was extracted to
  `src/util/token_bucket.rs` and is now reused by both limiters (zero
  duplication of the refill/peek/commit primitive).
- **Defaults** (per handoff): burst capacity 100, sustain
  16.67 req/s = 1000 req/min. Generous enough for a legitimate vision-loop
  addon fanning a frame to multiple backends, strict enough that a
  self-DoS bug can't saturate a shared yolo service.
- **Denial path**: returns `AbiError::QuotaExceeded` (code 11, reused from
  M1.W7 streaming subs). Each denial calls `note_denial_for_audit` which
  collapses the audit row to **at most one row per addon per 60 s
  window**, carrying `denied_count` for the in-window total — without
  this an attacker would turn a request DoS into an audit-log DoS.
  Audit row uses `risk_class='C'` (low-severity denial).

### Files

| File | Role | LOC |
|------|------|-----|
| `src/util/mod.rs` | New util module (token-bucket reuse home) | 9 |
| `src/util/token_bucket.rs` | Extracted `TokenBucket` (was inline in api/rate_limit.rs) | 85 |
| `src/services/service_call_rate_limit.rs` | Limiter + collapsed-audit map + tests | ~320 |
| `src/services/mod.rs` | Module registration | +1 line |
| `src/lib.rs` | `pub mod util;` | +1 line |
| `src/api/rate_limit.rs` | Use shared `TokenBucket` (removed local copy) | -48 / +2 |
| `src/addon/host_functions/service.rs` | Inserted rate-limit gate after `addon_id` capture | +35 |
| `tests/service_call_rate_limit.rs` | 5 integration tests | ~110 |

### Tests

Unit (in `src/services/service_call_rate_limit.rs`):
- `per_addon_burst_allowed` — 100 calls one addon all pass.
- `per_addon_burst_exceeded_denied` — 101-st call → `AddonLimit`.
- `different_addons_independent` — addon-a exhausted, addon-b still fresh.
- `eviction_at_hard_cap` — 11 k unique ids → ≤10 k entries after sweep.
- `refill_resumes_after_quota` — burst, sleep 1.1 s, allow again.
- `audit_collapse_first_emits_subsequent_skip` — first denial Emit, rest Skip.

Integration (`tests/service_call_rate_limit.rs`):
- `burst_of_100_allowed_then_101_denied` — default-shaped config.
- `addon_isolation` — addon-a denied while addon-b still allowed.
- `quota_refills_with_time` — 300 ms sleep restores 1.25 tokens at 5/s.
- `audit_emit_collapses_inside_window` — 1 emit + 1000 skips.
- `map_bounded_under_addon_id_churn` — 11 k addons, map ≤10 k.

All 11 tests green (`cargo test --features dashboard-api`).

### Decisions

- **TokenBucket reuse** — extracted to `src/util/token_bucket.rs`
  (~85 LOC, zero deps beyond `std`) rather than duplicating inline. Both
  rate limiters now share one implementation. E1 tests still green
  post-extraction.
- **Burst 100, refill 16.67/s** — matches handoff §5 spec (1000 req/min)
  plus a 6-second burst headroom for vision-loop fan-out patterns.
- **Audit window 60 s** — same as `api::dashboard::server` HTTP-side
  collapse (one row per addon per minute carrying denied_count).
- **No config knob yet** — `ServerConfig` does not currently expose a
  `[server.rate_limit]` section (the existing `RateLimitingConfig` lives
  at top level and targets API-key throttling). Adding a parallel
  per-addon block would be a new schema. Kept defaults hardcoded for P5;
  if operators need tuning, the singleton's `ServiceCallRateLimiter::new`
  takes a `ServiceCallRateLimitConfig` and the OnceLock can be replaced
  with a config-aware initialiser in a follow-up.

### Out of P5 scope

- Manifest-side `[runtime] rate_limit_per_min` per-addon override
  (mentioned in handoff §5). Deferred — the manifest schema work would
  carry its own DoD-12 review and is orthogonal to the host-side gate.
- Distributed rate limiting across mesh nodes — each node enforces
  locally. A coordinated addon spread across N nodes can do N× the
  budget. Acceptable for F1b (mesh is opt-in).

