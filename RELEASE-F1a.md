# TentaFlow F1a — TentaVision Basic — Release Notes

**Version:** 0.1.0-f1a
**Release Date:** 2026-05-16
**Status:** Beta (acceptance testing)

## Summary

F1a introduces the wasmtime addon system with three foundations:

1. **Addon ABI** — 30+ host functions (SQL, alias readonly, camera, streaming, recording, frame_url, signed URLs)
2. **TentaVision basic** — camera ingest via FakeFile (GStreamer), RawFrameRef + PickupToken security model
3. **Admin UI** — Services > Aliasy (M16 v1), Addon Bindings tab (M14), Install Wizard steps 1-3 (M15)

This is a backend-and-infrastructure release. The six TentaVision analytical
domains (D1-D6 — ADR, anomaly, abandoned-bag, re-id, attribute search, generic
detection) are out of scope here and ship in F1b/F2.

## Highlights

- 30+ ABI functions under the `camera` core feature flag
- HMAC-SHA256 signed URLs — `frame_url` (multi-use, 60-600s TTL) and `PickupToken`
  (one-shot, 30s TTL, scoped per service_call)
- DB schema at v22 (migrations v1 through v22 applied idempotently)
- ~124 tests (unit + integration + e2e + security) + 14 Criterion benches across
  M1.W4-W8
- Performance vs §17.8 of `tentavision-plan.md`: all measured targets PASS with
  16x to 11000x margin on micro-benchmarks (single-process, single-thread)
- Bidirectional permission model: `visibility` (private/restricted/public) +
  `allowed_consumers` per alias, plus `[[uses_alias]]` / `[[uses_model]]`
  declarations in consumer manifests

## Breaking Changes

- **Manifest TOML schema extended:** `[[uses_alias]]`, `[[uses_model]]` sections
  added; `visibility` and `allowed_consumers` fields added to `[[alias]]`.
  Manifests without these fields default to `visibility="private"` and parse
  cleanly.
- **Permission rename:** `alias.manage` renamed to `alias.read`. DB migration
  v13 updates existing rows in `addon_permissions` in-place.
- **Runtime alias CRUD ABI removed:** `alias_create_v1` and `alias_deactivate_v1`
  no longer exist. Alias lifecycle is exclusively driven by addon install and
  uninstall hooks (manifest `[[alias]]` block). Per project rules (no compat
  shims) the SDK wrappers `alias_create()` and `alias_deactivate()` were also
  removed.
- **Audit log schema:** `audit_log` rows now require `risk_class` (A/B/C/Unclassified)
  and optionally `related_claim_id`, `request_id`. Existing host-function call
  sites updated to call `audit_log_with_risk`.

## Migration Guide — teams-bot addon

teams-bot is the reference addon used throughout F1a development.

1. Edit `addons-pro/teams-bot/manifest.toml`:
   - Rename permission id `alias.manage` to `alias.read`.
2. Move runtime alias creation into the manifest `[[alias]]` block. Five aliases
   previously registered through `TEAMS_BOT_ALIASES` (now removed from
   `addon/mod.rs`) become five `[[alias]]` entries. Aliases are auto-created on
   install and deactivated on uninstall.
3. Remove any SDK calls to `alias_create()` and `alias_deactivate()` — these
   functions no longer exist in `addon-sdk`. Read-only `alias_get()` and
   `alias_list_owned()` remain available.
4. If teams-bot calls aliases owned by another addon, add `[[uses_alias]]`
   entries naming each external alias with `required = true/false` and a `reason`.

A one-shot DB migration backfilled existing `model_aliases` rows into
`model_alias_owners` during M1.W5 Chunk A. No manual data migration is required.

## Known Limitations (F1a Scope)

- **Camera vendors:** only `fake_file` (mp4 replay via GStreamer). RTSP/ONVIF/V4L2
  ship in F1b.
- **Recording:** PNG snapshots + MP4 segments (re-encode via x264). No
  ring-buffer or automatic retention — manual `recording_purge` only. Full
  retention engine ships in F3.
- **Authentication:** single-tenant admin. Multi-user RBAC ships in F2.
- **Mesh:** single-node only. HMAC signing keys are process-local (regenerated
  on restart). Multi-node key sync ships in F1b.
- **Audit chain:** flat `audit_log` table with `risk_class`. Merkle hash chain
  + `audit_verify_chain` API ship in F1b.
- **`service_call` rate limit:** not enforced in F1a. Ships in F1b.
- **Vector storage:** placeholder API (returns empty results). Full vector DB
  ships in F2.
- **UI:** screens M1, M3, M5, M6, M7, M11 are design mockups only. M14, M15
  (steps 1-3) and M16 v1 are implemented. Steps 4-6 of M15 (install wizard)
  are placeholders pending the `addonInstallConfigureRequest` backend message
  type. Custom addon UI components with signed iframes ship in F1c.
- **Real-world load:** all performance numbers are Criterion micro-benchmarks.
  The 24-hour soak test (`docs/SOAK_TEST.md`) is the production-load gate and
  is a manual acceptance step.

## Performance Targets (§17.8 of `tentavision-plan.md`)

All numbers from `cargo bench` Criterion runs in M1.W7 and M1.W8.

| Operation | Target | Measured (median) | Margin |
|-----------|--------|-------------------|--------|
| service_call overhead (pickup core model) | < 5 ms p99 | 7.72 µs | ~647x |
| stream_next (hot buffer) | < 1 ms p99 | 91 ns | ~11000x |
| pickup roundtrip 320x240 (in-process) | < 20 ms p99 | 147 µs | ~137x |
| pickup roundtrip 1280x720 (HTTP loopback) | < 20 ms p99 | 447 µs | ~44x |
| snapshot_save 320x240 PNG | < 50 ms p99 | 282 µs | ~177x |
| snapshot_save 1280x720 PNG | < 50 ms p99 | 3.12 ms | ~16x |
| signed URL issue/verify | < 1 ms p99 | 300-360 ns | ~2700x |
| pickup_token issue | < 1 ms p99 | 1.10 µs | ~900x |

Real-world QUIC + serialization overhead is expected to add 1-3 ms per
`service_call`, still well within the 5 ms / 20 ms budgets.

## Acceptance Status

- **M0 gate (manifest + SDK boilerplate + migrations):** CLOSED
- **M1 gate (DoD-1, DoD-2, DoD-5, DoD-6, DoD-7, DoD-8, DoD-10, DoD-11, DoD-12,
  DoD-13):** CLOSED (DoD recap in `notes/tentavision-f1a-implementation.md`
  M1.W8 section)
- **M2 gate (DoD-3, DoD-4, DoD-9):** CLOSED; M15 wizard steps 4-6 placeholder
  pending `addonInstallConfigureRequest` (see DoD-1 status in acceptance report)
- **M3 gate:** documentation + soak infrastructure ready. The 24-hour soak run
  itself (`docs/SOAK_TEST.md`) is a manual acceptance step.
- **DoD-15 (audit Merkle chain):** DEFERRED to F1b. F1a ships a flat audit log
  with `risk_class`; chain verification API is in the F1b scope.

See `notes/tentavision-f1a-acceptance-report.md` for the per-DoD breakdown.

## Upgrade Path to F1b

See `notes/tentavision-f1b-handoff.md` for prerequisites carried over, the
F1b scope (real RTSP/ONVIF, lab pilot, multi-node key sync, audit hash chain,
rate limiting), and design notes.

## License

Apache License 2.0 (see `LICENSE`).
