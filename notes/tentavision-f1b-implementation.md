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
| **P4** | Audit Merkle chain (DoD-15 full) | 1 week | pending |
| **P5** | service_call rate limit | 0.5 week | pending |
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
  after the existing `TrustedKeysSync` block, gated on the same
  `is_trusted` check + `last_sync_sent` cooldown (30 s).
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

## DB schema notes

Po P1.A schema cameras wygląda identycznie jak v21 z jedną zmianą:

```
vendor TEXT NOT NULL CHECK(vendor IN ('fake_file', 'rtsp', 'onvif'))
```

Wszystkie pozostałe kolumny, defaulty, CHECK'i innych pól oraz indeksy
są zreplikowane 1:1 z v21 (`CAMERAS_TABLE`).
