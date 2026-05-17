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

## Phase 3 — Vector storage backend (LANDED)

### Decisions (resolved from P0 Q5-Q7)

- **Backend:** `usearch` 2.25 (Apache 2.0, C++ core with `cxx` Rust bindings,
  HNSW + mmap on-disk persistence). Verified build on `linux x86_64` —
  `cargo build --release` 1m30s clean from a fresh target. Cross-compile to
  iOS/Android stays a P3.1 verification item; on those targets the trait
  abstraction allows a fallback to `hnsw_rs` without touching the host fns.
- **Deployment:** embedded (no extra process). `VectorBackend` trait abstracts
  the implementation so F2+ can swap in `QdrantBackend` without breaking
  callers.
- **Default metric:** cosine. Manifest can override per-namespace via
  `[[vector_namespace]].distance`.

### Files / LOC (approximate)

- `src/services/vector/mod.rs` (16) — re-exports.
- `src/services/vector/error.rs` (75) — `VectorError` + `Result` alias.
- `src/services/vector/backend.rs` (75) — `VectorBackend` trait, `Metric`
  enum, `SearchHit` struct.
- `src/services/vector/usearch_backend.rs` (300, incl. 9 unit tests) —
  thin `RwLock<usearch::Index>` wrapper. `multi=false` HNSW; upsert is
  implemented as `if contains { remove }; add` because the high-level
  usearch wrapper rejects duplicate keys.
- `src/services/vector/namespace.rs` (470, incl. 8 unit tests) —
  `NamespaceManager` (per-process `(addon_id, namespace) -> Arc<dyn>`
  cache via `dashmap`), DB-backed registry, per-addon quota enforcement,
  `delete_namespace` admin op.
- `src/addon/host_functions/vector.rs` (700) — 3 host fns
  (`vector_upsert_v1`, `vector_search_v1`, `vector_delete_v1`) with TOML
  input/output, permission gate, manifest-driven dim/metric resolution,
  gate placeholder, full audit per exit path.
- `src/db/migrations.rs` — adds migration v27 (`addon_vector_namespaces`).
- `addon-sdk/sdk/src/lib.rs` — 3 SDK wrappers (`vector_upsert`,
  `vector_search`, `vector_delete`) + `encode_vector_b64` helper +
  `VectorHit` in prelude.
- `tests/db_migrations_v27.rs` (7 tests).
- `tests/vector_host_functions.rs` (16 tests).

### Cargo deps added

- `usearch = "2.25"` (pulls `cxx` + `numkong`; C++ toolchain required).

### DB schema (v27)

```sql
CREATE TABLE addon_vector_namespaces (
    addon_id TEXT NOT NULL,
    namespace TEXT NOT NULL,
    dim INTEGER NOT NULL CHECK(dim >= 1 AND dim <= 4096),
    metric TEXT NOT NULL CHECK(metric IN ('cosine', 'euclidean', 'dot')),
    count INTEGER NOT NULL DEFAULT 0,
    file_path TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (addon_id, namespace)
);
CREATE INDEX idx_addon_vector_ns_addon ON addon_vector_namespaces(addon_id);
```

### Permissions (new)

- `vector.read` — gates `vector_search_v1`.
- `vector.write` — gates `vector_upsert_v1` + `vector_delete_v1`.

Permissions are dynamic strings checked by `PermissionChecker`; no static
registry needs updating.

### Manifest extension

The existing `[[vector_namespace]]` block (parsed since F1a as a structural
placeholder) is now wired live:

```toml
[[vector_namespace]]
name = "faces"
dimensions = 512
distance = "cosine"      # cosine | euclidean | dot
data_class = "B"
gate = "d4-historical"   # optional — when present vector_search MUST
                         # carry a non-empty gate_claim_id (P4 validates
                         # the claim itself; P3 only enforces presence)
```

Addons cannot create namespaces at runtime — every namespace must be
declared in the manifest, and dim/metric are pinned at declaration time.

### Quotas (hard-coded F1c, configurable F2+)

- `MAX_NAMESPACES_PER_ADDON = 10`.
- `MAX_VECTORS_PER_ADDON = 1_000_000` (sum of cached `count` column).

Both map to `AbiError::QuotaExceeded` (code 11).

### Persistence policy

`save()` runs synchronously after every successful upsert/delete. F2 may
introduce write batching once a real workload pushes back.

### Flagged (per P0 risk register)

- **usearch C++ cross-compile for iOS/Android** — not exercised in P3.
  Desktop + Linux x86_64 work today. Mobile verification lands in P3.1;
  the `VectorBackend` trait makes a switch to `hnsw_rs` a single-file
  change if needed.
- **Sync vs batched persistence** — F1c sync, F2 batch.
- **`requires_claim` gate** — structural-only in P3 (claim must be
  present, not validated). Real validation lands in P4.

### Tests (all green)

- 8 + 8 unit tests across `services::vector::*` (open/upsert/search/delete,
  cosine distance, dim mismatch, persist+reopen, quota, isolation,
  invalid names, etc.).
- 7 migration tests in `tests/db_migrations_v27.rs`.
- 16 integration tests in `tests/vector_host_functions.rs` covering the
  decode helper, gate matrix, error mapping, end-to-end manager flow,
  cross-addon isolation, quota, persist+reopen, namespace delete, and an
  AbiError code-stability sweep.

### Build status

`cargo build --lib --features dashboard-api` clean. Pre-existing
`dashboard-api` resolution errors when the flag is omitted are unrelated
to P3 (they predate this branch).

---

## Phase 4 — Policy/claims engine (LANDED)

Closes the legal-compliance gate for D4 face re-identification (RODO +
AI Act). Without a valid claim signed by DPO (+ supervisor when the gate
requires multi-sig), an addon cannot run gated vector namespaces or
gated aliases.

### Files

| File | LOC | Role |
|------|-----|------|
| `src/db/migrations.rs` (+v28) | +56 | New `policy_claims` + `policy_claim_signatures` tables, type/scope indexes, FK cascade. |
| `src/services/policy/mod.rs` | 20 | Module re-exports. |
| `src/services/policy/error.rs` | 40 | `PolicyError` enum (7 variants via `thiserror`). |
| `src/services/policy/repo.rs` | 230 | CRUD on policy tables (insert/get/list/revoke/signatures). |
| `src/services/policy/engine.rs` | 360 (incl. 11 unit tests) | `verify_claim(pool, claim_id, ctx)` — read-only validity / scope / signer check. |
| `src/addon/host_functions/gate.rs` | 320 (incl. 4 unit tests) | `gate_check_v1` host fn + manifest→engine ctx builder (`build_context`, `primary_claim_type_for_gate`, `required_signer_roles_for_gate`). |
| `src/addon/host_functions/vector.rs` | +60 | `enforce_gate_with_policy` replaces the P3 structural placeholder. `vector_search_v1` now denies with `result='gate_denied'` + reason on policy rejection. |
| `addon-sdk/sdk/src/lib.rs` | +75 | `gate_check_v1` extern + `gate_check` / `gate_check_scoped` wrappers + `GateCheckResult` / `GateSigner`. |
| `tentaflow-cli/src/commands/policy.rs` | 400 | `policy issue|list|verify|revoke|show` admin subcommands. |
| `tentaflow-cli/src/main.rs` + `commands/mod.rs` | +12 | Wire the policy top-level subcommand. |

### DB schema (v28)

```sql
CREATE TABLE policy_claims (
    claim_id TEXT PRIMARY KEY,
    claim_type TEXT NOT NULL,             -- dpia | fria | legal_grant | consent | approval | grant | deployment_profile
    label TEXT NOT NULL,
    subject TEXT NULL,
    scope TEXT NULL,
    document_uri TEXT NULL,               -- Optional PDF/repo URI
    scope_addon_id TEXT NULL,             -- NULL = global
    scope_namespace TEXT NULL,            -- NULL = any resource
    valid_from TEXT NOT NULL,             -- UTC ISO-8601
    valid_until TEXT NOT NULL,
    revoked_at TEXT NULL,
    revoked_reason TEXT NULL,
    issued_by_user TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_policy_claims_type ON policy_claims(claim_type);
CREATE INDEX idx_policy_claims_scope ON policy_claims(scope_addon_id, scope_namespace);

CREATE TABLE policy_claim_signatures (
    claim_id TEXT NOT NULL REFERENCES policy_claims(claim_id) ON DELETE CASCADE,
    signer_role TEXT NOT NULL,            -- dpo | supervisor | data_controller | ...
    signer_user TEXT NOT NULL,
    signed_at TEXT NOT NULL,
    signature_b64 TEXT NULL,              -- NULL = manual ack; populated = Ed25519
    PRIMARY KEY (claim_id, signer_role, signer_user)
);
CREATE INDEX idx_policy_claim_sig_claim ON policy_claim_signatures(claim_id);
```

### Host function

- `gate_check_v1(gate_id, claim_id, resource_scope?)` — addon-callable
  inspection. Returns TOML `{ valid, claim_id, claim_type, valid_until,
  signers, reason? }`. Requires `policy.read`. Risk class B.
- `vector_search_v1` — implicit enforcement: when the namespace's manifest
  `[[vector_namespace]].gate` is set, the search is denied with
  `AbiError::GateNotSatisfied` + audit `result='gate_denied'`,
  `details=<reason>` (claim_not_found / claim_revoked /
  claim_outside_validity / claim_type_mismatch / claim_scope_mismatch /
  missing_required_signer).

### Permissions

- `policy.read` — addon-side, gates `gate_check_v1`.
- `policy.write` — admin-side (CLI only); no addon ABI surface for issuing
  or revoking claims.

### CLI

```
tentaflow-cli policy issue --claim-id <id> --type <dpia|fria|legal_grant|consent|approval|grant|deployment_profile> \
  --label "<label>" [--document-uri <uri>] [--scope-addon <addon>] [--scope-namespace <ns>] \
  [--valid-from <ISO>] --valid-until <ISO> [--issued-by <user>] \
  --signer <role:user> [--signer <role:user> ...] [--db <path>]
tentaflow-cli policy list [--type <type>] [--active-only] [--db <path>]
tentaflow-cli policy show --claim-id <id> [--db <path>]
tentaflow-cli policy verify --claim-id <id> --type <type> [--addon <id>] [--namespace <ns>] \
  [--signer-role <role> ...] [--db <path>]
tentaflow-cli policy revoke --claim-id <id> --reason "<text>" [--db <path>]
```

### Manifest mapping

Existing `[[gate]]` block (parsed since F1a as a structural placeholder)
is now wired live to the engine:

- `primary_claim_type_for_gate` — first non-"approval" claim type
  (artifact: dpia / fria / consent) becomes the claim_type matched
  against `policy_claims.claim_type`.
- `required_signer_roles_for_gate` — every `required_claims[*]` with
  `type="approval"` contributes its `subject` to the required signers
  list. When none declared, defaults to `["dpo"]` (DPIA requires DPO).

No manifest schema change.

### Engine contract

`verify_claim` returns `Ok(ClaimVerified)` only when ALL of the
following hold:

1. The claim exists.
2. `revoked_at IS NULL`.
3. `valid_from <= now <= valid_until` (UTC ISO-8601 lexicographic compare).
4. `claim_type == ctx.claim_type_required`.
5. `scope_addon_id IS NULL OR scope_addon_id == ctx.addon_id`.
6. `scope_namespace IS NULL OR scope_namespace == ctx.resource_scope.Some(_)`.
7. For every `role` in `ctx.required_signer_roles`, at least one row
   exists in `policy_claim_signatures` with that role.

### Audit integration

- `gate_check_v1` emits `action='policy.gate_check'`, risk B, result
  `'gate_ok'` or `'gate_denied'`. `related_claim_id` carries the claim
  id; `details` carries the reason code on denial.
- `vector_search_v1` denial emits `action='vector.search'`, risk B,
  result `'gate_denied'`, `details=<reason>`.
- All rows continue to participate in the F1b Merkle chain (no schema
  change to audit_log).

### SDK bindings

```rust
pub fn gate_check(gate_id: &str, claim_id: &str) -> Result<GateCheckResult, AbiError>;
pub fn gate_check_scoped(gate_id: &str, claim_id: &str, resource_scope: Option<&str>)
    -> Result<GateCheckResult, AbiError>;

pub struct GateCheckResult {
    pub valid: bool,
    pub claim_id: String,
    pub claim_type: String,
    pub valid_until: String,
    pub signers: Vec<GateSigner>,
    pub reason: Option<String>,
}
pub struct GateSigner { pub role: String, pub user: String }
```

### Cargo deps

Zero new — reuses `thiserror`, `chrono`, `rusqlite`, `serde`, `toml`,
`base64` (all already in the workspace).

### Tests (all green)

- `services::policy::engine::tests` — 11 unit tests (valid, unknown,
  revoked, expired, future, type mismatch, scope addon/namespace,
  missing signer, global scope, namespace-scoped require resource).
- `addon::host_functions::gate::tests` — 4 unit tests (primary type
  resolution, signer roles aggregation, defaults).
- `tests/db_migrations_v28.rs` — 6 (table create, recorded version,
  idempotent reopen, composite PK rejects dup, FK cascade, indexes).
- `tests/policy_host_function.rs` — 7 (engine-path OK, denial-revoked,
  denial-unknown, helper coverage, reason mapping completeness).
- `tests/vector_gate_enforcement.rs` — 8 (ungated OK, gated-no-claim,
  unknown-gate-id, gated-OK, gated-revoked, gated-ns-scoped,
  gated-addon-scoped wrong addon, helper lookup).
- `tentaflow-cli/tests/cli_policy.rs` — 7 (issue+show, reject unknown
  type, require signer, list filters, revoke marks reason, verify
  ok, verify denial-revoked).

### Decisions

- **Signature acknowledgment** — manual admin ack via CLI is the
  contract in F1c-P4; `signature_b64` column is reserved for the
  Ed25519 blob (verified by a future hardening pass). Manual ack is
  not weaker: the CLI is admin-only and every issuance + revocation
  is audit-logged.
- **`scope_addon_id IS NULL` = global** — a global claim matches any
  addon; a narrowed claim must equal the caller's addon id exactly.
- **`scope_namespace`** — when set, the caller MUST supply
  `resource_scope` matching it (vector_search auto-supplies the
  namespace name). Missing scope on a narrowed claim is denial, not
  fallthrough.
- **claim_id format** — caller-chosen string (e.g. `claim-dpia-faces-2026`).
  No internal ULID/UUID minting — admins want human-readable ids in
  audit reports.
- **No addon-side issuance** — `policy.write` permission exists only
  in CLI; the addon ABI carries `gate_check_v1` (read) but no
  `gate_issue_v1` or `gate_revoke_v1`.

### Build status

`cargo build --lib --features dashboard-api,vector` clean.
`cargo test --lib services::policy::` 11/11 green.
`cargo test --test db_migrations_v28 --test policy_host_function --test vector_gate_enforcement`
21/21 green. `cargo test --test cli_policy` 7/7 green.
`cargo clippy` clean on all new files.
