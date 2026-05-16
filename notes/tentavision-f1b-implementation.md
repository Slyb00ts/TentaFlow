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

## DB schema notes

Po P1.A schema cameras wygląda identycznie jak v21 z jedną zmianą:

```
vendor TEXT NOT NULL CHECK(vendor IN ('fake_file', 'rtsp', 'onvif'))
```

Wszystkie pozostałe kolumny, defaulty, CHECK'i innych pól oraz indeksy
są zreplikowane 1:1 z v21 (`CAMERAS_TABLE`).
