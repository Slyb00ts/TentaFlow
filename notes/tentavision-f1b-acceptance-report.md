# TentaVision F1b — Acceptance Report

**Wersja:** 1.0
**Data:** 2026-05-17
**Release tag:** v0.1.0-f1b (proponowany — manual step, patrz koniec dokumentu)
**Powiązane:**
- `notes/tentavision-plan.md` §15 — roadmap F1a/F1b/F1c
- `notes/tentavision-f1b-implementation.md` — pełen trace P1–P5
- `notes/tentavision-f1a-acceptance-report.md` — DoD baseline (F1a)
- `RELEASE-F1b.md` — release notes
- `notes/tentavision-f1c-handoff.md` — następna faza

## Streszczenie

### Phase status

| Faza | Status | Uwagi |
|------|--------|-------|
| P1.A — DB v23 CHECK rtsp/onvif | ✅ DONE | commit `0361d73` |
| P1.B — RTSP connector + reconnect | ✅ DONE | commits `e301071` + `459d33c` |
| P1.C — AES-GCM credentials + rotacja | ✅ DONE | commits `3071b36` + `6ffa700` |
| P1.D — ONVIF WS-Discovery + test_connection | ✅ DONE | commits `3acf1b8` + `bb10dca` (SSRF) |
| E1 — Audit security hardening | ✅ DONE | commits `72c762a` + `e64ca82` |
| E2 — TLS 1.3 + HSTS + mTLS pickup | ✅ DONE | commit `d50064c` |
| P3.A — Key persistence (3 klucze) | ✅ DONE | commits `0789a04` + `7d31139` |
| P3.B — Mesh HMAC key sync | ✅ DONE | commits `793f197` + `99a5ad1` |
| P3.C — Cross-node frame pickup | ✅ DONE | commits `db226d3` + `200974d` + `28d4706` |
| P4 — Audit Merkle chain (DoD-15) | ✅ DONE | commit `753d0fe` |
| P5 — service_call rate limit | ✅ DONE | commit `bea2bb3` |
| P2 — Lab pilot 4 fizyczne kamery | ⊘ DEFERRED | wymaga hardware ONVIF/RTSP — F1c gate |
| P6 — Bug bash z F1a 24h soak | ⊘ DEFERRED | wymaga real soak run — F1c gate |

### DoD cumulative (F1a + F1b)

| Status | Liczba DoD |
|--------|-----------|
| ✓ PASS | 12 (F1a: 11 + DoD-15 z P4) |
| ⚠ PARTIAL | 3 (DoD-1, DoD-5, DoD-6 — bez zmian vs F1a) |
| ⊘ DEFERRED | 2 (DoD-14 pełny sweep — F2; DoD-15 zamknięte przez P4) |
| ✗ FAIL | 0 |
| **Total** | **17** |

---

## P1.A — DB v23 CHECK rtsp/onvif

**Status:** ✅ DONE
**Commit:** `0361d73`
**Weryfikacja:** `tentaflow-core/tests/db_migrations_v23.rs` (6 testów):
preserves fake_file, allows rtsp, allows onvif, rejects unsupported,
recreates indexes, recorded in `_migrations`. SQLite table-rebuild pattern
(create-new + insert-select + drop + rename) z `DROP TABLE IF EXISTS
cameras_new` dla idempotencji częściowo nieudanego runu. Indeksy v21
(`idx_cameras_camera_id_active` partial unique, `idx_cameras_owner`,
`idx_cameras_status`) odtwarzane po rebuild.
**Notes:** `SUPPORTED_VENDORS` w `addon/host_functions/camera.rs` w P1.A
pozostaje `["fake_file"]` — rozszerzenie czeka na gotowy connector (P1.B/D).

## P1.B — RTSP connector

**Status:** ✅ DONE
**Commits:** `e301071` (connector), `459d33c` (credential redaction +
test fixes)
**Weryfikacja:** `tentaflow-core/src/services/camera_ingest/rtsp.rs` —
pipeline `rtspsrc ! rtph264depay ! h264parse ! avdec_h264 ! videoconvert !
video/x-raw,format=RGB ! appsink`. Reconnect: exp backoff 1/2/4/8/max 60 s
z ±20% jitter, reset przy FLOWING. Integracja z `CameraSession` +
`Supervisor` z F1a M1.W6. `SUPPORTED_VENDORS` rozszerzony o `"rtsp"`.
System deps udokumentowane w `docs/SOAK_TEST.md`:
`gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-libav
gstreamer1.0-rtsp` (Debian/Ubuntu); `brew install gstreamer` (macOS).
**Notes:** Lab z 4 fizycznymi kamerami (P2) — deferred. Real-world
throughput / appsink queue depth pod sustained load do zmierzenia w F1c
gate.

## P1.C — AES-GCM credentials + rotacja

**Status:** ✅ DONE
**Commits:** `3071b36` (cipher + storage + ABI), `6ffa700` (CLI rotacja +
helpers)
**Weryfikacja:**
- `services::camera_ingest::credentials` — AES-256-GCM (nonce 12 B,
  tag 16 B), klucz z `<tentaflow_home>/keys/cameras.key` (32 B, 0o600 na
  Unix), atomic write, env override `TENTAFLOW_CAMERAS_KEY`, singleton
  `credentials_cipher()`.
- `camera_add_v1` przyjmuje opcjonalny `credentials_b64` (base64 z
  `user:pass`); RTSP connector dekryptuje przed każdym
  `build_rtsp_pipeline`. Helper `overlay_credentials(url, "user:pass")`
  odmawia overlay gdy URL już zawiera credentials.
- `camera_credentials_rotate_v1` — real implementacja: walidacja b64,
  encrypt z bieżącym master key, audit `result=ok` z
  `details=blob_len=X cleared=bool` (nigdy plaintext).
- CLI `tentaflow-cli camera rotate-key`: generuje nowy master key,
  re-encrypt wszystkich blobów w transakcji, archiwum starego klucza
  `cameras.key.YYYYMMDD-HHMMSS`, atomic rename.
- Testy: 13 unit (`credentials.rs::tests`) + 6 integration
  (`tests/credentials_rotation.rs`).
**Notes:** Credential redaction w logach (P1.B `459d33c`) eliminuje URL
z `user:pass@host` z trace/debug — codex review iteracja zaaplikowana.

## P1.D — ONVIF discovery

**Status:** ✅ DONE
**Commits:** `3acf1b8` (discovery + test_connection), `bb10dca` (SSRF
hardening)
**Weryfikacja:** WS-Discovery UDP multicast 239.255.255.250:3702, probe
SOAP envelope z `NetworkVideoTransmitter`, parse XAddrs, query Device
service po manufacturer + model. Real `camera_discover_v1` (F1a zwracało
pusty `Vec`). Real `camera_test_connection_v1` — RTSP DESCRIBE/SETUP z
timeoutem. `SUPPORTED_VENDORS` rozszerzony o `"onvif"`.
**SSRF guard (`bb10dca`):** `onvif_test_connection` wymusza ścieżkę
URL pod `/onvif/` — addon nie może użyć `camera_test_connection_v1` do
HEAD'owania arbitralnych targetów HTTP na LAN. Test
`test_onvif_test_connection_forces_onvif_path` pokrywa 3 branche (bare
host, ONVIF sub-service preserved, arbitrary path rewritten).
**Notes:** One-click "wybierz discovered camera → add" UX czeka na F1c
(wizard krok dla ONVIF). W F1b admin dodaje ONVIF ręcznie przez URI.

## E1 — Audit security hardening

**Status:** ✅ DONE
**Commits:** `72c762a` (rate limit per-IP + global + CORS + audit IP),
`e64ca82` (path containment)
**Weryfikacja:**
- Per-IP HMAC token bucket w `api::rate_limit` (`TokenBucket` extracted
  do `src/util/token_bucket.rs` w P5 — reused).
- CORS whitelist w `api::dashboard::server`.
- Path containment guard na każdym disk read (frame URLs, recording
  URLs) — chroni przed `..` escape spoza `<tentaflow_home>`.
- `audit_log.source_ip` capture w HTTP path (poprzednio NULL dla
  ścieżek API).
**Notes:** Closes DoS regression z F1a soak observation (per-IP nie było
gate'owane przed E1).

## E2 — TLS 1.3 + HSTS + mTLS pickup

**Status:** ✅ DONE
**Commit:** `d50064c`
**Weryfikacja:** Server config wymusza TLS 1.3 only (rustls cipher suite
filter). HSTS header `max-age=31536000; includeSubDomains` always-on.
Optional `[server.mtls.pickup]` — gdy enabled, pickup HTTP wymaga
client cert z pinningiem. 2-tier transport documented w `CLAUDE.md`:
mesh control plane = mTLS, pickup HTTP = TLS 1.3 + opcjonalny mTLS.
**Notes:** Breaking — klienci TLS 1.0/1.1/1.2 odrzuceni. Production
clients muszą support TLS 1.3.

## P3.A — Single-node key persistence

**Status:** ✅ DONE
**Commits:** `0789a04` (keys + atomic rotation), `7d31139` (file watcher
+ 2-key window)
**Weryfikacja:** Trzy klucze HMAC pod `<tentaflow_home>/keys/`:
- `pickup_token.key` (32 B, 0o600) — 30 s one-shot.
- `frame_url.key` (32 B, 0o600) — frame URLs (max 10 min TTL).
- `recording_url.key` (32 B, 0o600) — recording URLs (max 1 h TTL).

Lazy `OnceLock` singletons w `services::mod.rs`. Atomic generation:
`getrandom::fill` + `tmp → rename + chmod 0o600`. Crash recovery na
startup: `<name>.key.new` obok live promotowany (durable commit
marker), `<name>.key.staging` odrzucany. Operator rotation:
`tentaflow-cli keys rotate <name>` — staging → .new → live. 2-key
in-memory window: stary klucz jako verify-only secondary przez
`max_ttl + 5 s`, restart wymagany żeby nowy klucz signował.
**Notes:** Restart impact: pre-P3.A każdy restart unieważniał wszystkie
outstanding signed URLs i pickup tokens (process-local `OsRng`).
Post-P3.A klucze przeżywają restart.

## P3.B — Mesh HMAC key sync

**Status:** ✅ DONE
**Commits:** `793f197` (wire + pool + dispatch), `99a5ad1` (rotation
grace + trust integration tests)
**Weryfikacja:**
- Wire: `MESH_MSG_HMAC_KEYS_SYNC = 0x44`, payload
  `HmacKeysSyncPayload { from_node_id, keys: [HmacKeyEntry] }`. Każdy
  entry: scope (`"pickup_token"` / `"frame_url"` / `"recording_url"`),
  `current_key` 32 B, opcjonalny `previous_key` (rotation grace),
  diagnostic 8-byte `key_id`.
- `IrohMeshManager::send_hmac_keys_sync` + event
  `IrohMeshEvent::HmacKeysSyncReceived`. Dispatcher w `mesh::pipeline`
  reużywa trust gate jak `TrustedKeysSync` (sender musi być
  `is_trusted`).
- `services::mesh_keys::MeshKeyPool` — `RwLock<HashMap<NodeId,
  PerPeerKeys>>` singleton. Hot path verify: jeden read lock, collect
  candidate set, drop lock, constant-time HMAC compare na każdym
  kandydacie (no early-exit timing leak).
- Verify order: lokalne klucze (full inflight + one-shot contract)
  PIERWSZE; mesh fallback runs tylko na `InvalidSignature`.
- Lifecycle: `handle_peer_connected` re-advertises na każdym trusted
  reconnect (OUTSIDE `last_sync_sent` cooldown — HMAC keys mogą się
  rotować, lazy propagation OK). `handle_peer_disconnected` drops
  scope dla peera. `TrustRevokedReceived` propagation też drops.
- Testy: 8 unit (`services::mesh_keys`) + 4 integration
  (`tests/mesh_key_sync_integration.rs` — pickup cross-node, signed
  URL cross-node × 2 scopes, rotation grace, trust-boundary contract).
**Notes:** One-shot semantics dla mesh-issued pickup tokenów: receiver-side
replay protection NIE jest w P3.B (token wciąż HMAC-verifies + 30 s
expiry, więc attacker window jest wąski). B-side replay closed w P3.C-2.

## P3.C — Cross-node frame pickup

**Status:** ✅ DONE
**Commits:** `db226d3` (P3.C-1 wire), `200974d` (P3.C-2 logic + replay +
DB v24), `28d4706` (P3.C-3 HTTP integration)
**Weryfikacja:**
- **P3.C-1 wire:** `MESH_MSG_FRAME_PROXY_REQUEST = 0x45`,
  `MESH_MSG_FRAME_PROXY_RESPONSE = 0x46`. Payloady
  `FrameProxyRequestPayload { raw_ref, request_id }` i 3-wariant enum
  `FrameProxyResponsePayload { Found / NotFound / Unavailable }`.
  Eventy `IrohMeshEvent::FrameProxyRequestReceived` /
  `FrameProxyResponseReceived` po pre-trust gate.
- **P3.C-2 logic:** `PickupTokenIssuer::verify_only_with_source` zwraca
  `(TokenPayload, VerifySource::{Local, Peer(node_id)})` w
  constant-time (zawsze evaluuje oba candidate sets).
  `mesh_inflight_consume` — pierwszy cross-node consume zapisuje
  timestamp, kolejne → `AlreadyConsumed` przez `2 × TTL`. DB v24 dodaje
  nullable `frame_pickup_log.source_node_id` dla audit.
- **P3.C-3 HTTP:** `api::frame_pickup::verify_pickup_headers` wydzielony
  z `handle_pickup`. Hyper handler w `dashboard/server.rs` dispatchuje
  per `VerifySource`. `Local` — lokalna ścieżka. `Peer(node_id)` —
  `mesh_inflight_consume` (replay guard) → `frame_proxy::client::fetch_from_peer`
  (5 s timeout). Mapping outcome → HTTP:

  | Outcome | HTTP | `frame_pickup_log.result` | `source_node_id` |
  |---|---|---|---|
  | `Found{bytes,meta}` | 200 + body + width/height/pf/ts | `ok` | `Some(peer)` |
  | `NotFound` | 404 | `frame_purged` | `Some(peer)` |
  | `Unavailable{reason}` | 503 + `Retry-After: 5` | `upstream_unavailable` | `Some(peer)` |
  | Timeout (5 s) | 503 + `Retry-After: 5` | `upstream_unavailable` | `Some(peer)` |
  | Replay (B-side) | 403 | `replay` | `Some(peer)` |
  | `mesh_unavailable` | 503 + `Retry-After: 5` | `upstream_unavailable` | `Some(peer)` |

- Hardening: 1 KiB GET body cap (P1.C-2), signed-URL rate limit (E1),
  universal security headers (E2) działają BEFORE verify split — cross-node
  i local pickup ten sam rate-limit + body-size + header surface.
- Testy: `tests/mesh_frame_proxy_dispatch.rs` (2 — wire round-trip via 2
  real `IrohMeshManager`), `tests/frame_pickup_cross_node.rs` (5 —
  outcome → HTTP / log_result mapping + `mesh_inflight_consume` first-ok
  / second-replay). Istniejące `tests/streaming_pickup.rs` (12) +
  `streaming_pickup_e2e.rs` (6) dalej zielone.
**Notes:** Mesh single-message cap 16 MiB. 4K RGB24 (24.8 MiB) > cap —
chunked transport (P3.D) deferred. HD (1920×1080 = 6.2 MiB) i mniej
transmitują clean.

## P4 — Audit Merkle chain (DoD-15)

**Status:** ✅ DONE
**Commit:** `753d0fe`
**Weryfikacja:**
- **Schema v25:** ALTER `audit_log` ADD `prev_hash BLOB` + `hash BLOB`.
  Idempotent przez `PRAGMA table_info(audit_log)`. Rust-step migration
  (`audit_log_add_merkle_chain_columns`). F1a rows pozostają NULL —
  verifier liczy je jako `legacy_unchained`.
- **Canonical serialization:** `\0`-joined UTF-8 z każdej DB-visible
  kolumny w fixed order (`audit/chain.rs::canonical_row_bytes`).
  SHA-256 unsalted — każdy reader DB może verify, no secret material.
- **Writer paths updated:** `audit_log_with_risk` (host-fn primary),
  `AuditLogger::flush` (batched), `db/repository.rs::log_audit` /
  `log_audit_full` / 3 `audit_*_within_tx`. SELECT(latest hash) +
  INSERT pod tym samym DB lock (linearizable). Timestamp
  pre-rendered (`YYYY-MM-DD HH:MM:SS`) — nie `datetime('now')` —
  ten sam string trafia do DB i do hash input. Severity zawsze
  explicit (no schema DEFAULT reliance).
- **Verifier:** `audit::verify::verify_chain` zwraca `VerifyReport`
  z `chained_ok`, `legacy_unchained`, `Vec<TamperedRow { id, kind }>`
  z `TamperKind`: `PrevHashMismatch`, `HashMismatch`,
  `NullHashAfterChainStart`, `MalformedHashBlob`.
- **CLI:** `tentaflow-cli audit verify [--db-path PATH]`. Exit 0
  (clean), 1 (tamper, per-row reason na stdout), 2 (DB error).
- **Testy:** 5 unit (`audit::chain::tests` — canonical determinism,
  collision via NUL separator, prev_hash chaining) + 9 unit
  (`audit::verify::tests` — empty/genesis/10-row/modified hash/modified
  content/inserted/deleted/legacy NULL/NULL after chain start) +
  4 integration (`tests/security_audit_chain.rs`).
**Notes:** Performance: extra SELECT na hot rowid index + SHA-256 nad
~200-500 B = sub-50 µs na 100 k-row table. Bez bench regression w F1a
soak harness. Real-time tamper alerting deferred (operator cron z CLI).

## P5 — service_call rate limit

**Status:** ✅ DONE
**Commit:** `bea2bb3`
**Weryfikacja:**
- **Per-addon bucket:** `DashMap` keyed by `addon_id`, bounded przez
  LRU (`MAX_ADDON_ENTRIES = 10 000`) + idle sweep (`IDLE_EVICT_AFTER =
  600 s`).
- **TokenBucket reuse:** wyciągnięte do `src/util/token_bucket.rs`
  (~85 LOC, std-only). Reused przez `api::rate_limit` (per IP) i
  `services::service_call_rate_limit` (per addon). E1 testy zielone
  post-extraction.
- **Defaults:** burst 100, sustain 16.67 req/s = 1000 req/min. Match
  handoff §5 + 6-second burst headroom.
- **Denial:** `AbiError::QuotaExceeded` (code 11, reuse z M1.W7).
  `note_denial_for_audit` collapse → max 1 row per addon per 60 s
  window, carrying `denied_count`. `risk_class='C'`.
- **Testy:** 6 unit + 5 integration (`tests/service_call_rate_limit.rs`)
  — burst allowed, 101st denied, addon isolation, refill, audit
  emit+skip, map bounded under churn.
**Notes:** Manifest `[runtime] rate_limit_per_min` override deferred do
F1c (separate schema review). Distributed rate limit cross-mesh
deferred — każdy node enforces locally.

---

## ⊘ DEFERRED phases (manual hardware/runtime)

### P2 — Lab pilot 4 fizyczne kamery

**Status:** ⊘ DEFERRED do F1c-opening gate
**Powód:** Wymaga 4 fizycznych kamer ONVIF/RTSP (Hikvision/Axis lub
podobne) na LAN, 30 fps × 24 h sustained, restart mid-stream test.
Nie infrastruktura CI — wymaga ręcznego setupu lab.
**Co jest gotowe code-side:** P1.B (RTSP connector z exp backoff
reconnect), P1.D (ONVIF discovery + test_connection), GStreamer deps
dokumentowane w `docs/SOAK_TEST.md`. Akceptacja "4 cameras × 30 fps
× 24 h z reconnect tolerance" wymaga wykonania w F1c gate.

### P6 — Bug bash z F1a 24h soak

**Status:** ⊘ DEFERRED do F1c-opening gate
**Powód:** Wymaga real 24 h soak run wg `docs/SOAK_TEST.md` (M3.W14
F1a — manual acceptance). Issues otwierają się dopiero z observed
behaviour pod sustained load. Memory leak deep-dive z `dhat-rs` (RSS
growth > 5% / 24 h), FD leak (lsof monotonic) — wymagają konkretnych
sygnałów z soaka.
**Co jest gotowe:** P3.B / P3.C / P4 / P5 są unit + integration tested.
Bug bash F1c-open będzie konsumować F1a + F1b soak razem.

---

## DoD cumulative breakdown (F1a + F1b)

Per-DoD szczegóły z F1a niezmienione poza DoD-15 (closed via P4).
Skrócona recap:

| DoD | F1a status | F1b delta | Cumulative |
|-----|-----------|-----------|-----------|
| DoD-1 wizard M15 | ⚠ PARTIAL (kroki 4-6 placeholder) | bez zmian | ⚠ PARTIAL |
| DoD-2 6 aliasów po install | ✓ PASS | — | ✓ PASS |
| DoD-3 M14 6 aliasów + 4 storage cards | ✓ PASS | — | ✓ PASS |
| DoD-4 M16 v1 inline edit | ✓ PASS | — | ✓ PASS |
| DoD-5 service_call e2e | ⚠ PARTIAL (WASM-guest e2e do M2.W11) | bez zmian | ⚠ PARTIAL |
| DoD-6 FakeFile → pickup pipeline | ⚠ PARTIAL (warstwowo OK, full e2e M2.W11) | bez zmian | ⚠ PARTIAL |
| DoD-7 recording snapshot + URL | ✓ PASS | — | ✓ PASS |
| DoD-8 per-addon SQLite + migrations | ✓ PASS | — | ✓ PASS |
| DoD-9 permission denied → audit | ✓ PASS | — | ✓ PASS |
| DoD-10 PickupToken replay → 403 | ✓ PASS | rozszerzone o B-side replay (P3.C-2) | ✓ PASS |
| DoD-11 path traversal blokowany | ✓ PASS | rozszerzone o ONVIF SSRF guard (P1.D) | ✓ PASS |
| DoD-12 SQL injection guard | ✓ PASS | — | ✓ PASS |
| DoD-13 performance §17.8 | ✓ PASS (bench) | brak regresji (E1/P4/P5 mierzone) | ✓ PASS |
| DoD-14 24 error codes | ⊘ DEFERRED do F2 | +1 (RateLimited z P5) — łącznie 14/24 | ⊘ DEFERRED |
| **DoD-15 audit Merkle chain** | ⊘ DEFERRED do F1b | **✓ PASS (P4)** | **✓ PASS** |
| DoD-16 migration idempotent | ✓ PASS | v23/v24/v25 idempotent | ✓ PASS |
| DoD-17 release notes + handoff | ✓ PASS | F1b release + F1c handoff | ✓ PASS |

---

## Podsumowanie krytyczne

**Co działa produkcyjnie w F1b (incremental nad F1a):**
- Production camera vendors RTSP + ONVIF (discovery + test_connection)
- Credentials encryption AES-GCM + rotacja CLI (atomic + archive)
- Persistent HMAC keys (3 scope) z 2-key rotation window
- Multi-node mesh: HMAC keys distributed across trusted peers
- Cross-node frame pickup (proxy via mesh stream, B-side replay)
- Audit Merkle chain (DoD-15 closed, CLI verify)
- Per-addon service_call rate limit (1000 req/min default)
- TLS 1.3 + HSTS + optional mTLS pickup
- Per-IP + global HTTP rate limit (DoS regression fix)

**Co świadomie odłożone (deferred z F1b):**
- P2 lab pilot 4 fizyczne kamery — F1c-opening gate
- P6 bug bash z 24h soak — F1c-opening gate
- ONVIF one-click camera add (wizard step) — F1c
- Mesh broadcast-on-rotate (HMAC key push without reconnect) — F1c
- Frame proxy chunked transport (>16 MiB / 4K) — F1c (P3.D)
- Real-time audit tamper alert — F3 (z HSM/TSA)
- Manifest `[runtime] rate_limit_per_min` override — F1c
- Distributed rate limit cross-mesh — F2+

**Co dalej (z F1c handoff):**
- Addon UI signed iframe components (tv-video-grid, tv-zone-editor, ...)
- Policy/claims engine (DPIA/FRIA gates dla D4)
- Vector storage full
- Flow invoke (DAG operators)
- Multi-tenant RBAC — F2

---

## Manual step — git tag

Po sign-off ze stakeholderami + zaliczonych P2 (lab) + P6 (soak):

```bash
git tag -a v0.1.0-f1b -m "TentaFlow F1b — RTSP/ONVIF + mesh sync + audit chain"
git push origin v0.1.0-f1b
```

**UWAGA:** Pełne v0.1.0-f1b ship-ready jest po manualnym domknięciu
P2 + P6. Code-only acceptance (11 phase commits) jest gotowy do
preview-tag / RC tag jeżeli stakeholder zaakceptuje deferred bracket.
