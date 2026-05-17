# TentaVision F1c — implementation status

Tracks landing of the F1c phases (UI iframe + signature verify + vector
backend) against the P0 design (`notes/tentavision-f1c-p0-design.md`).

---

## Phase 1 — UI iframe foundation (LANDED)

**Branch:** `flow-engine-stage-3d-v1.5`
**Decisions baked in (from P0 §D):** Q1 Minimal sandbox, Q2 UI-only arch,
Q3 strict JSON-shape validation, Q4 auto-derive caps from `host_permissions`.

### Files

| File | Role |
|------|------|
| `tentaflow-core/www/js/components/tf-addon-ui-frame/tf-addon-ui-frame.js` | `<tf-addon-ui-frame>` web component — sandboxed iframe, ui.init dispatch, status overlay |
| `tentaflow-core/www/js/addon-ui-host.js` | Parent harness — registry, message dispatch, permission gate, backend routing |
| `tentaflow-core/www/js/addon-ui-host/bridge-schema.js` | Action registry + per-action input validators + action→permission map |
| `tentaflow-core/www/test-fixtures/addon-ui-mock/dashboard.html` | Mock addon bundle (postMessage client reference) |
| `tentaflow-core/www/test-fixtures/addon-ui-demo.html` | Manual demo page (live permission flipping) |
| `tests/e2e/addon-ui-iframe.spec.js` | Playwright e2e — 8 tests (sandbox, ui.init, EPERM, EBADREQ, EUNIMPL, EUNKNOWN_ACTION, ui.notify, unmount) |
| `tests/e2e/helpers/static-www-server.js` | Tiny static server so this spec runs without the tentaflow binary |

### Actions wired in P1

| Action | Backend | Permission scope | Notes |
|--------|---------|------------------|-------|
| `alias.list_owned` | `modelAliasListRequest` (binary WS, filtered by `owner_addon_id`) | `alias.read` | Live |
| `camera.list` | EUNIMPL | `camera.read` | Awaits admin host-fn surface |
| `camera.snapshot` | EUNIMPL | `camera.snapshot` | Awaits admin host-fn surface |
| `vector.search` | EUNIMPL | `vector.read` | Lands in P3 (vector backend) |
| `ui.notify` | local (parent `tf-addon-toast` CustomEvent) | none | Live |

### Error codes (postMessage `response.error.code`)

- `EPERM` — addon lacks required permission scope
- `EBADREQ` — envelope or payload failed schema validation
- `EUNKNOWN_ACTION` — action not in registry
- `EUNIMPL` — registered action whose backend lands in a later phase
- `EINTERNAL` — backend error or dispatch failure

### Security posture

- `sandbox="allow-scripts"` only — no popups, no same-origin, no top-nav
- iframe gets unique opaque origin → `connect-src 'none'` in bundle CSP is
  enforceable per-bundle
- Identity binding: `iframe.contentWindow === event.source`, addonId looked
  up from parent-controlled registry (never from message payload)
- Bundle delivery: `blob:` URL constructed by parent from `bundleHtml`
  (P2 will source from addon FS after Ed25519 verify; P1 accepts any
  string for dev/test)

### Not in P1 (deferred)

- Ed25519 signature verify (P2)
- Real bundle serving from addon FS (P2)
- Vector storage backend (P3)
- Camera admin host-fn surface (separate scope)

---

## Phase 2 — Ed25519 signature verify + manifest [[ui_component]] parse

Status: implemented. See `notes/tentavision-f1c-p0-design.md` §A.8.

### What landed

- `[publisher]` manifest block (`tentaflow-core/src/addon/manifest.rs`):
  `PublisherInfo { ed25519_public_key, label, contact }`. Cross-section
  invariant: any manifest declaring `[[ui_component]]` must also carry
  `[publisher]`; bare `[publisher]` w/o UI components is allowed.
- DB migration v26 (`tentaflow-core/src/db/migrations.rs`): table
  `trusted_publishers(key_b64 PK, label, added_at, added_by_user,
  contact)` + index on `label`. NOT seeded — default-deny.
- Signature module `tentaflow-core/src/addon/signature.rs`:
  `verify_ui_component_bundle()` does trust-store lookup, base64
  decode, SHA-256(bundle) → Ed25519 verify, typed `SignatureError`.
  Accepts `ed25519:<base64>` prefix used in manifests.
- Install hook in `lifecycle::install`: iterates `ui_components`
  before any DB write; first failure aborts install and writes audit
  row `action='addon.ui_signature_verify'`,
  `details='denied: ...; publisher_pk=<8-char prefix>'`. OK path emits
  one row per component. Full pk is never written to logs.
- Repository helpers: `TrustedPublisher` struct + insert/list/remove/
  is_trusted (`tentaflow-core/src/db/repository.rs`).
- CLI (`tentaflow-cli/src/commands/addon.rs`) four subcommands:
  `addon trust-key <key_b64> --label <l> [--contact <c>] [--db <p>]`,
  `addon list-trusted [--db <p>]`,
  `addon untrust-key <key_b64> [--db <p>]`,
  `addon verify-bundle <bundle> --publisher-key <pk> --signature <s> [--db <p>]`.
  Signing tool (admin packaging) deferred to packaging workflow.

### Tests

- `signature.rs::tests` — 8 units (valid, valid w/prefix, wrong sig,
  tampered, untrusted, bad pk format, bad sig format, empty bundle).
- `manifest.rs::chunk_c_validation_tests` — +3 (ui_component w/o
  publisher, bad pk length, empty label).
- `tests/db_migrations_v26.rs` — 5 (columns, recorded, empty default,
  PK uniqueness, idempotent reopen).
- `tests/addon_signature_install.rs` — 6 (install OK + audit, bad sig
  rejected, untrusted rejected, parse-time reject of ui_component
  without publisher, publisher-only manifest parses, sanity).
- `tentaflow-cli/tests/cli_addon_trust.rs` — 4 (trust→list→untrust
  round-trip, reject bad pk length, verify-bundle happy, verify-bundle
  untrusted publisher).

### Compat notes

- `tests/fixtures/broken_manifest_path_traversal.toml` and three
  inline TOML literals in `tests/addon_manifest_parsing.rs` gained
  `[publisher]` blocks (otherwise they hit the new cross-section
  validator before the rule they were meant to exercise).
- No real addon under `tentaflow-core/addons/*` declares
  `[[ui_component]]`, so the install hook does not affect any
  installed addon today.

## Phase 3 — Vector storage backend
Not started.

## Phase 3 — Vector storage backend
Not started.
