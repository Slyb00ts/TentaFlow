# TentaFlow F1b — Production camera ingest + audit chain + mesh sync

**Version:** 0.1.0-f1b
**Release Date:** 2026-05-17
**Status:** Beta (acceptance testing — P2 lab pilot + P6 soak-bug-bash are manual gates)
**Builds on:** F1a v0.1.0-f1a (TentaVision basic)

## Summary

F1b closes the "real cameras + multi-node + tamper-evident audit" gap left by
F1a. Five workstreams ship together:

1. **Production camera vendors** — RTSP (full ingest) and ONVIF (discovery +
   probe). F1a shipped only `fake_file` (mp4 replay).
2. **Encrypted camera credentials** — AES-256-GCM at rest with a 2-key
   rotation CLI; plaintext never touches disk or logs.
3. **Multi-node mesh** — HMAC signing keys are now persisted, shared across
   trust-paired peers, and cross-node frame pickups proxy bytes back to the
   source node with B-side replay protection.
4. **Audit Merkle chain** — every `audit_log` row carries `prev_hash` + `hash`;
   `tentaflow-cli audit verify` detects tamper. Closes DoD-15 (F1a deferral).
5. **Service-call rate limit** — per-addon token bucket (1000 req/min default)
   guards shared backend services from a single buggy or malicious addon.

Plus two security hardenings consumed by every path above: TLS 1.3 + HSTS +
optional mTLS pickup (E2), and unified per-IP + global HTTP rate limits +
audit IP capture (E1).

## Highlights

- 2 new camera vendors: `rtsp` (full streaming, exponential backoff reconnect
  with ±20% jitter) and `onvif` (WS-Discovery UDP 239.255.255.250:3702;
  one-click add via discovery deferred to F1c).
- AES-256-GCM credentials encryption sealed under `<tentaflow_home>/keys/cameras.key`
  with archived-rotation CLI (`tentaflow-cli camera rotate-key`).
- Three new persistent HMAC keys: `pickup_token.key`, `frame_url.key`,
  `recording_url.key` (each 32 B, mode 0600, atomic staging → `.new` → live
  with crash-recovery on startup).
- Mesh HMAC key sync — `MESH_MSG_HMAC_KEYS_SYNC = 0x44` re-advertises on every
  trusted `PeerConnected` event; a token minted on node A verifies on node B
  without sharing on-disk keys.
- Cross-node frame pickup proxy — `MESH_MSG_FRAME_PROXY_REQUEST = 0x45` /
  `MESH_MSG_FRAME_PROXY_RESPONSE = 0x46`. B-side replay protection via
  `mesh_inflight_consume` (first ok, subsequent → `AlreadyConsumed` for
  `2 × TTL`). Audit row carries `frame_pickup_log.source_node_id`.
- Audit Merkle chain (DB v25): `hash = SHA256(canonical(row) || prev_hash)`,
  unsalted SHA-256 so any DB reader can verify with no secret material.
- `service_call` per-addon token bucket: 100 burst, 16.67 req/s sustained
  (= 1000 req/min). Denials return `AbiError::QuotaExceeded` and audit rows
  collapse to ≤1 per addon per 60 s (carrying `denied_count`).
- HTTP per-IP + global rate limit (DoS regression fix), audit IP capture,
  CORS whitelist, path-containment guard on every disk read.
- TLS 1.3 only (no 1.2 fallback), HSTS always-on, optional mTLS pickup
  (`[server.mtls.pickup]`).
- 13 phase commits in F1b (P1.A-D + E1 + E2 + P3.A-C + P4 + P5),
  ~9.5 k LOC code, ~50 new tests, every codex review iteration applied.

## Breaking Changes

- **DB schema:** v23 (cameras vendor CHECK extended to `rtsp` / `onvif`) +
  v24 (`frame_pickup_log.source_node_id` nullable) + v25 (`audit_log.prev_hash`,
  `audit_log.hash`). All three are additive (CHECK widened + ALTER ADD COLUMN
  patterns). Existing rows preserved; F1a audit rows count as
  `legacy_unchained` in the verifier and are NOT flagged as tamper.
- **TLS 1.3 only** (was 1.2/1.3 in F1a). Clients pinned to TLS 1.0/1.1/1.2
  are rejected. Production HTTPS clients must support TLS 1.3.
- **`service_call_v1` may return `AbiError::QuotaExceeded`** when the calling
  addon exceeds 1000 req/min. Addon authors must handle this error (code 11,
  reused from the M1.W7 streaming subscription path).
- **Optional mTLS pickup** (`[server.mtls.pickup]`) defaults to disabled.
  Enabling in production hard-blocks pickup requests without a pinned client
  cert — operators must roll out client certs before flipping the flag.

## Migration Guide

F1b is a drop-in DB-additive upgrade for any addon that already targets F1a.
No manifest changes required.

If you operate a multi-node mesh:

1. Confirm `<tentaflow_home>/keys/` directory is owned by the tentaflow
   process user (0700 recommended). On first startup post-upgrade each node
   will generate three 32 B key files locally.
2. Peer trust pairings carry over from F1a. HMAC key sync auto-runs on the
   next `PeerConnected`; no operator action.
3. After upgrade, run `tentaflow-cli audit verify` once to seed the chain
   from any pre-existing `audit_log` history (legacy rows stay `NULL` —
   chain begins with the first new row after upgrade).

If you call `service_call` in a tight loop:

- Default budget is 1000 req/min per addon. Vision-loop fan-out gets a
  100-call burst on top. If you need more, file a config request — the
  per-addon manifest override is deferred (out of P5 scope) but the limiter
  singleton accepts a `ServiceCallRateLimitConfig` for follow-up wiring.

## Known Limitations (F1b Scope)

- **ONVIF GetStreamUri:** WS-Discovery returns vendor + model + XAddrs, but
  the "click discovered camera → `camera_add_v1` with vendor=onvif" UX is
  F1c. F1b operators add ONVIF cameras manually by URI.
- **Mesh key sync is lazy on rotate.** `tentaflow-cli keys rotate <name>`
  flips the local issuer to a 2-key in-memory window (`ttl + grace`) so old
  tokens still verify; peers re-pick the fresh key on their next
  `PeerConnected` advertise. An explicit broadcast-on-rotate path is
  deferred.
- **Frame proxy size limit:** mesh single-message cap is 16 MiB. A 4K RGB24
  frame (24.8 MiB) exceeds the cap; chunked transport is P3.D (deferred).
  HD (1920×1080 RGB24 = 6.2 MiB) and below transit cleanly.
- **Audit chain is detection-only.** Tamper is detected on verify but no
  real-time alert fires; operators are expected to run
  `tentaflow-cli audit verify` from nightly cron. Real-time alerting is
  out of F1b; HSM + RFC 3161 TSA per-row signing is F3.
- **Per-addon rate-limit manifest knob.** Defaults are hardcoded (1000
  req/min, burst 100). The handoff §5 `[runtime] rate_limit_per_min` field
  is deferred to F1c — manifest-schema changes carry their own review surface.
- **Distributed rate limit.** Each node enforces locally. A coordinated
  addon spread across N nodes can do N× the budget. Acceptable for F1b
  (mesh is opt-in for production deployments).
- **Lab cameras pilot (P2)** and **24 h soak bug bash (P6)** are deferred
  to manual acceptance gates — both require physical infrastructure
  (4 ONVIF/RTSP cameras on a LAN, 24 h continuous run on production-shape
  hardware). The code paths they exercise (P1.B/D, P3.B/C) are unit +
  integration tested.

## DoD Status (F1a + F1b cumulative)

| Status | F1a | F1b delta | Cumulative |
|--------|-----|-----------|-----------|
| ✓ PASS | 11 | +1 (DoD-15) | 12 |
| ⚠ PARTIAL | 3 | unchanged | 3 |
| ⊘ DEFERRED | 3 | -1 (DoD-15 closed) | 2 |
| ✗ FAIL | 0 | — | 0 |
| **Total** | **17** | | **17** |

DoD-15 (audit Merkle chain) moved PASS via P4. DoD-5 / DoD-6 / DoD-14 remain
PARTIAL on the same grounds noted in `tentavision-f1a-acceptance-report.md`
(WASM-guest e2e and full 24-code sweep depend on F2/F3 trigger paths). See
`notes/tentavision-f1b-acceptance-report.md` for the per-phase breakdown.

## Files (phase commits)

| Phase | Commit | Scope |
|-------|--------|-------|
| P1.A  | `0361d73` | DB v23 — `cameras.vendor` CHECK + `rtsp` / `onvif` |
| P1.B  | `e301071` + `459d33c` | RTSP connector + exp-backoff reconnect + credential redaction |
| P1.C  | `3071b36` + `6ffa700` | AES-GCM credentials + 2-key rotation CLI |
| P1.D  | `3acf1b8` + `bb10dca` | ONVIF WS-Discovery + real `test_connection` (SSRF-hardened) |
| E1    | `72c762a` + `e64ca82` | Per-IP + global rate limit, CORS whitelist, path containment, audit IP |
| E2    | `d50064c` | TLS 1.3 only + HSTS + mTLS pickup + 2-tier transport doc |
| P3.A  | `0789a04` + `7d31139` | Key persistence + watcher (3 keys) |
| P3.B  | `793f197` + `99a5ad1` | Mesh HMAC key sync (`MESH_MSG_HMAC_KEYS_SYNC = 0x44`) |
| P3.C-1 | `db226d3` | Frame proxy wire + events (`0x45` / `0x46`) |
| P3.C-2 | `200974d` | Proxy logic + B-side replay + DB v24 (`source_node_id`) |
| P3.C-3 | `28d4706` | HTTP cross-node pickup integration |
| P4    | `753d0fe` | Audit Merkle chain — DB v25 + verifier + CLI |
| P5    | `bea2bb3` | `service_call` per-addon rate limit + collapsed audit |

## Performance Notes

No bench regression vs F1a (§17.8 numbers carry over). New paths measured:

- Audit chain write — one extra `SELECT hash … ORDER BY id DESC LIMIT 1` + one
  SHA-256 over ~200-500 B per audit row. Sub-50 µs on a 100 k-row table in
  the verify-tests bench. Well below the rusqlite + WAL fsync floor; not a
  hot spot in the F1a soak harness.
- Token bucket (`util::token_bucket`) — shared between `api::rate_limit` (per
  IP) and `services::service_call_rate_limit` (per addon). Single-µs scale,
  zero-alloc on the hot path.
- Cross-node pickup — 5 s timeout, `Retry-After: 5` on `Unavailable` or
  timeout. Replay protection is `2 × TTL` (60 s default), so the attacker
  window matches the local one-shot contract.

## Upgrade Path → F1c

See `notes/tentavision-f1c-handoff.md` for the next phase: addon UI signed
iframe components, policy/claims engine for D4 gating, vector storage,
flow invoke + DAG operators, ONVIF one-click camera add, and the deferred
P2 lab pilot + P6 soak as F1c-opening gates.

## License

Apache License 2.0 (see `LICENSE`).
