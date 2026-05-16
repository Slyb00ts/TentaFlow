# TentaVision F1b — Handoff from F1a

**F1a Status:** Released v0.1.0-f1a (acceptance pending — manual sign-off + soak)
**F1b Goal:** Production camera ingest (RTSP/ONVIF) + lab pilot + audit chain
**Estimated effort:** ~7-8 weeks for one senior, ~4-5 weeks for a 2-person team
**Related:**
- `notes/tentavision-plan.md` §15 (roadmap F1 split)
- `notes/tentavision-f1a-acceptance-report.md` (carry-overs)
- `RELEASE-F1a.md` "Known Limitations"

## Pre-conditions met by F1a

- Addon ABI stable: 30+ host functions under feature `camera`, SDK wrappers, sdk_version compatibility check
- DB schema at v22 (cameras, recordings, alias_calls, frame_pickup_log,
  model_alias_visibility/consumers, addon_uses_alias/model,
  addon_migrations_applied, audit_log with risk_class)
- FakeFile camera vendor as reference implementation (GStreamer
  `filesrc ! decodebin ! videoconvert ! appsink`)
- Streaming bus (bounded mpsc, capacity 100, drop oldest + Drop{count}) +
  frame_storage LRU (1024 frames/node default)
- PickupToken security model (HMAC SHA256, one-shot, 30s TTL, scoped per service)
- Two SignedUrlIssuer registries (frame multi-use 60-600s, recording multi-use
  60-3600s)
- Recording PNG snapshot + MP4 segment with atomic tmp+rename + sha256 integrity
- Admin UI baseline: M16 v1 (services/aliases), M14 readonly (addon bindings),
  M15 wizard steps 1-3
- Bidirectional permission model (visibility + consumers + uses_alias/uses_model)

## F1b Scope

### 1. Real RTSP/ONVIF camera vendors

- DB migration v23: extend `cameras.vendor` CHECK constraint to include
  `rtsp`, `onvif`. Existing `fake_file` rows untouched.
- New module `services/camera_ingest/rtsp.rs` with GStreamer pipeline
  `rtspsrc location=... ! rtph264depay ! h264parse ! avdec_h264 ! videoconvert !
  video/x-raw,format=RGB ! appsink`. Reuse `CameraSession` + supervisor scaffold
  from F1a (M1.W6).
- `camera_discover_v1` (F1a returns empty Vec) → implement WS-Discovery
  UDP multicast 239.255.255.250:3702, reply parsing, list of `DiscoveredCamera`
  with manufacturer + model + URI.
- `camera_test_connection_v1` (F1a fake_file only) → real RTSP probing
  (SETUP/DESCRIBE round-trip with timeout).
- `camera_credentials_rotate_v1` (F1a noop) → real rotation: AES-GCM re-encrypt
  `cameras.credentials_encrypted` with new master key version, audit row in
  `audit_log` with `risk_class='B'`.
- Credentials encryption: AES-GCM with key from
  `~/.tentaflow/keys/cameras.key` (256-bit, generated at first run, rotated
  via CLI). The `cameras.credentials_encrypted` BLOB column is already present
  from F1a (M1.W6 Chunk A) — currently unused.

### 2. Lab cameras pilot (4 physical cameras)

- Setup: 4 cameras across 2 vendors (Hikvision, Axis or similar).
- Integration test on real network: connect, stream 30 fps for 1 h, restart one
  camera mid-stream, verify reconnect.
- Network resilience: GStreamer bus Error → exponential backoff (1s, 2s, 4s,
  8s, max 60s) with ±20% jitter. Reset backoff on successful connection.
- Bandwidth profiling: per-camera RTSP throughput (Mbps), decode CPU/GPU usage,
  appsink queue depth under sustained load.
- Acceptance: 4 cameras × 30 fps × 24 h with reconnect tolerance.

### 3. Multi-node mesh (key sync)

- HMAC signing key sharing across mesh peers. Currently both `PickupToken`
  signing key and the two SignedUrlIssuer keys are process-local (generated at
  startup, restart invalidates). Multi-node needs:
  - Shared key store (sealed at rest, distributed via QUIC mesh control plane)
  - Token issued on node A must verify on node B
  - Restart of node B without losing in-flight tokens of <30s TTL
- Frame storage replication strategy decision: replicate frames to peer nodes
  vs. proxy pickup requests back to source node. Recommended: proxy (cheaper,
  no extra memory pressure). Open design note.
- Recording URL signing key persistence — long-TTL (up to 3600s) so restart
  invalidates customer-facing URLs in F1a. F1b should persist this key to
  `~/.tentaflow/keys/signed_urls.key` (sealed).

### 4. Audit chain hash (DoD-15 full)

- Add `prev_hash` and `hash` columns to `audit_log` (DB migration v24).
- Genesis row inserted on first run.
- Each new row computes `hash = SHA256(prev_hash || canonical(row))`.
- Public API `audit_verify_chain(from_ts, to_ts)` walks rows, recomputes hashes,
  returns first mismatch or success.
- Tamper-detection test (M2.W11 deferred): manually UPDATE one historical row,
  call `audit_verify_chain`, expect failure with row id.

### 5. service_call rate limit

- Per-addon rate limit (default 1000 req/min, configurable per addon via
  manifest `[runtime]` section).
- Token bucket implementation (refill rate + burst capacity), per-addon
  in-memory state.
- Limit denials emit `AbiError::RateLimited` (one of the 24 codes already
  defined in M0.W2) and `audit_log` row with `risk_class='A'`,
  `result='rate_limited'`.

### 6. Bug bash from F1a soak

- Issues opened from the 24-hour soak run (`docs/SOAK_TEST.md`, M3.W14 manual
  acceptance step).
- Memory leak deep-dive with `dhat-rs` if RSS growth > 5% / 24 h.
- FD leak investigation if `lsof` count grows monotonically.

### 7. Pełne pokrycie 24 error codes (carry-over DoD-14)

F1a pokrywa 13/24 wariantów. Pozostałe 11 wymagają trigger paths z featurów
F2/F3 (PolicyClaimMissing, VectorNamespaceMissing itp.), więc pełen sweep
przesuwa się do F2. W F1b dodajemy tylko: `RateLimited` (z punktu 5),
`AuditChainCorrupt` (z punktu 4).

## Design notes

### RTSP architecture

Pipeline:
```
rtspsrc location=rtsp://user:pass@host/stream !
  rtph264depay ! h264parse ! avdec_h264 !
  videoconvert ! video/x-raw,format=RGB ! appsink
```

Reconnect strategy: GStreamer bus Error → exponential backoff (1s, 2s, 4s, 8s,
max 60s) with ±20% jitter. Reset backoff on successful FLOWING state.

Credentials: AES-GCM (nonce + ciphertext + tag) with 256-bit key from
`~/.tentaflow/keys/cameras.key`. CLI command `tentaflow-cli keys rotate
--scope cameras` for rotation.

### ONVIF discovery

WS-Discovery probe (UDP multicast 239.255.255.250:3702). Probe message =
SOAP envelope with NetworkVideoTransmitter type. Reply parsing extracts XAddrs
(device service URI). Probe XAddrs via Device service for manufacturer + model
metadata.

`camera_discover_v1` returns `Vec<DiscoveredCamera>`. Admin selects rows in UI
(M2 wizard new step) + provides credentials → standard `camera_add_v1` path
with vendor=`onvif`.

### Migration path F1a → F1b

DB migration v23: extend `cameras.vendor` CHECK constraint.
DB migration v24: `audit_log` add `prev_hash`, `hash` columns + genesis row +
backfill hashes for existing rows (one-shot script in `db/migrations.rs`).

Manifest schema: no breaking changes in F1b. New optional `[runtime]
rate_limit_per_min` field; old manifests default to 1000.

## Estimated effort breakdown

| Task | Effort |
|------|--------|
| RTSP basic + reconnect | 2 weeks |
| ONVIF discovery | 1 week |
| Lab cameras pilot (integration + flaky-network handling) | 1 week |
| Audit chain (DoD-15) | 1 week |
| Service_call rate limit | 0.5 week |
| Multi-node mesh key sync | 2 weeks |
| Bug bash from soak | 0.5-1 week |
| **Total F1b** | **~7-8 weeks** (one senior) |

## Out of F1b scope

These remain in later phases (per `tentavision-plan.md` §15):

- Custom UI components with Ed25519 signed iframes — F1c
- Policy / claims engine, vector store full, flow invoke — F2
- Evidence signing (HSM + TSA RFC 3161), full recording retention engine — F3
- PostgreSQL backend — F8
- D1-D6 inference logic (ADR, anomaly, abandoned-bag, re-id, attribute search,
  generic detection) — model integrations across F2-F4
