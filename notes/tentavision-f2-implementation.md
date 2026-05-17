# F2 — Implementation Plan

Stan po F1c (commit `fd3aa2b`): UI iframe + Ed25519 + trust-store, vector backend usearch + namespaces + quoty, policy/claims engine z DPO multi-sig, flow_runtime DAG z 6 operatorami (Source/Predict/Threshold/Branch/Aggregate/Sink) ale `camera.*` source jeszcze odrzucany, ONVIF GetStreamUri + wizard backend, per-user audit attribution (`actor_user_id`). DB na v31. Branch: `flow-engine-stage-3d-v1.5`.

## Cele F2

F1c zamknął minimum prawne (claims engine) i fundament runtime (flow_runtime + vector storage), ale pozostawił szereg dziur uniemożliwiających realne wdrożenie produkcyjne D1 (ADR end-to-end) i D4 (re-id z gating). F2 zamyka te dziury w trzech wymiarach jednocześnie:

1. **Realne dane wpięte w runtime**: flow `camera.*` Source operator pobiera klatki z prawdziwego camera-ingest (RTSP/ONVIF z F1b), zamiast wracać `Invalid camera source spec`.
2. **Pełny multi-tenant RBAC + per-org isolation**: F1c dał tylko `actor_user_id` w audit. F2 daje organy, role, per-org addon installs, per-org policy claims. Wymóg twardy żeby D4 mógł działać u wielu klientów na jednym węźle.
3. **Operacyjne polerki**: M16 v2 (dropdown z autocomplete + round-robin), gate-check cache (perf hot path), ONVIF Profile M (analytics metadata) + Profile G (forensics), polished install wizard step z M15 discovered cameras, mesh broadcast-on-rotate (HMAC keys push bez czekania), concurrency cap konfigurowalny per addon.

D4 RODO klauzula informacyjna w M10 oraz Profile G traktowane jako stretch — patrz "Deferred to F3" jeśli capacity nie wystarczy.

## Decyzje PM wymagane przed startem

| # | Pytanie | Domyślnie zakłada plan | Alternatywa |
|---|---------|------------------------|-------------|
| 1 | Kolejność faz: krótkie wygrane czy fundament? | **Fundament najpierw** — P1 multi-tenant RBAC. Wszystko po nim (M16 v2, policy cache, ONVIF M/G, camera source) konsumuje `org_id`/`user_id` jako kontekst. Robienie inaczej oznaczałoby retrofit `org_id` do każdej tabeli i każdej audit-row drugi raz. | "Quick wins first" (M16 v2 + cache jako P1-P2, RBAC jako P4) — odrzucone, generuje rework |
| 2 | Multi-tenant scope w F2 | **Pełen org isolation** — tabela `organizations`, `org_id` FK w `users`/`addon_installations`/`policy_claims`/`cameras`/`recordings`/`addon_vector_namespaces`/`audit_log`/`frame_pickup_log`. Per-org addon install. | Tylko `user_roles` per addon (~600 LOC) — ale wtedy D4 nie ma sensownego deploymentu enterprise w F2 |
| 3 | ONVIF Profile M vs G | **Profile M w F2, Profile G defer do F3**. M (metadata analytics) = 600-800 LOC. G (recording/search) = ~1500 LOC, naturalnie zazębia się z F3 "API-8 recording full". | Oba w F2 (+1500 LOC, +2 tygodnie) — odrzucone |
| 4 | Real camera Source operator — model dostarczania | **Per-frame push z bounded broadcast channel**. Flow rejestruje subskrypcję na `camera_id`; camera-ingest broadcastuje `RawFrameRef` do wszystkich subskrybentów; backpressure = drop-oldest z audit. Reuse istniejącego `frame_storage` LRU. | Per-batch pull (Source wywołuje `frame_pickup` co N ms) — odrzucone, łamie semantykę live |
| 5 | D4 RODO informacyjna | **Generator w M10 admin UI** — admin wybiera template, system wypełnia automatycznie pola (org name, DPO, retention z schematu, modele z policy_claims), eksport jako PDF + publikacja do `<tentaflow_home>/legal/` z signed URL. | Ręczna edycja Markdown — odrzucone, niespójne między klientami |

## Phases overview

| Phase | Title | Scope (skrót) | LOC est | Depends on | Priority |
|-------|-------|---------------|---------|------------|----------|
| P1 | Multi-tenant RBAC + org isolation | `organizations`, `roles`, `org_memberships`, `org_id` FK w 8 tabelach, RBAC middleware, per-org addon install | ~2200 | — | **High** |
| P2 | M16 v2 + service_list + round_robin | host fn `service_list_v1` + `node_resources_get_v1`, M16 dropdown + autocomplete, router `round_robin` strategy | ~700 | P1 | High |
| P3 | gate_check_cache + concurrency cap configurable | in-memory cache w `verify_claim`, manifest `[runtime] max_concurrency` + `rate_limit_per_min` override | ~500 | P1 | High |
| P4 | Real camera Source operator | `flow_runtime` Source: subskrypcja broadcast channel z `services::camera_ingest`, mapping na `Frame{camera_id, ts, raw_ref}` | ~900 | P1, P3 | High |
| P5 | Mesh broadcast-on-rotate (carry-over) | Push `MESH_MSG_HMAC_KEYS_SYNC` natychmiast po `keys rotate`, bez czekania na `PeerConnected` | ~350 | — | Med |
| P6 | ONVIF Profile M (analytics metadata) | Metadata service binding, PullPoint events parser, host fn `camera_metadata_subscribe_v1`, manifest `[[metadata_consumer]]` | ~800 | P1 | Med |
| P7 | Polished install wizard step + M15 panel | Frontend M15 "Discovered cameras" panel, wpięcie `camera_discover_v1` + `onvif_get_stream_uri_v1` | ~600 | P1 | Med |
| P8 | D4 RODO klauzula informacyjna — M10 generator | Templates Handlebars, generator PDF (genpdf/printpdf), admin UI w M10, signed URL publikacja | ~700 | P1 | Low |

**Total LOC**: ~6750. **Phases**: 8. **Estimated duration**: 6-8 tygodni single dev / 4-5 tygodni team 2 osób.

---

## Phase P1 — Multi-tenant RBAC + org isolation

### Background
F1c P7 dodał per-user audit attribution. System pozostaje jednoorganizacyjny. Addon installs są procesowo globalne. Niemożliwe wdrożenie u dwóch klientów na jednym węźle bez kopiowania całej instalacji.

### What lands
- Tabela `organizations(org_id PK, name, slug UNIQUE, created_at, contact_email, dpo_contact, retention_policy_json, status)`.
- Tabela `roles(role_id PK, name, permissions_json)` — preseed: `org_admin`, `org_operator`, `org_viewer`, `dpo`, `supervisor`.
- Tabela `org_memberships(org_id, user_id, role_id, granted_at, granted_by, PK(org_id, user_id))`.
- Dodanie kolumny `org_id TEXT` w: `users`, `addon_installations`, `policy_claims`, `cameras`, `recordings`, `addon_vector_namespaces`, `audit_log`, `frame_pickup_log`.
- Backfill: tworzy `organizations` row `org-default` i przypisuje wszystkie istniejące rekordy.
- RBAC middleware w `api/unified_server.rs`: każdy request resolve `(user_id, org_id, role)` z session.
- Per-org addon install: storage path `<tentaflow_home>/orgs/<org_id>/addons/<addon_id>/`. Migracja v32 przenosi katalogi.
- Audit emit gains `org_id` everywhere.
- CLI: `tentaflow-cli org create|list|invite|remove`, `tentaflow-cli user assign-role`.

### Files / LOC
| File | LOC | Role |
|------|-----|------|
| `src/db/migrations.rs` (v32) | +180 | 9 tabel/kolumn altered, backfill Rust step |
| `src/services/org/{mod,repo,error}.rs` | ~400 | CRUD organizations + memberships |
| `src/services/rbac/{mod,middleware,permissions}.rs` | ~550 | OrgContext extractor, permission matrix |
| `src/api/unified_server.rs` | +120 | RBAC middleware integration |
| `src/api/binary_protocol/*.rs` | +200 | OrgContext threaded przez message handlers |
| `src/addon/lifecycle.rs` | +150 | per-org install path, per-org sandbox |
| `src/addon/host_functions/*.rs` | +180 | org_id w audit emit (sweep przez ~12 host fns) |
| `tentaflow-cli/src/commands/org.rs` | ~280 | CLI subcommands |
| `tentaflow-core/www/js/admin/orgs.js` + HTML | ~250 | Admin UI "Organizations" |
| Tests: `tests/multi_tenant_*.rs` | ~700 | Org isolation E2E + RBAC matrix |
| Cleanup pre-org code | -100 | Usuwamy "default org" implicit |

### DB schema changes (v32)
```sql
CREATE TABLE organizations (
    org_id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    slug TEXT NOT NULL UNIQUE,
    contact_email TEXT,
    dpo_contact TEXT,
    retention_policy_json TEXT,
    status TEXT NOT NULL DEFAULT 'active',
    created_at TEXT NOT NULL
);
CREATE TABLE roles (
    role_id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    permissions_json TEXT NOT NULL
);
CREATE TABLE org_memberships (
    org_id TEXT NOT NULL REFERENCES organizations(org_id),
    user_id TEXT NOT NULL REFERENCES users(user_id),
    role_id TEXT NOT NULL REFERENCES roles(role_id),
    granted_at TEXT NOT NULL,
    granted_by TEXT NOT NULL,
    PRIMARY KEY (org_id, user_id)
);
-- ALTER ADD COLUMN org_id w 8 tabel + indeksy + backfill
```

### Permissions / risk_class
- `org.read`, `org.write`, `org.admin`
- `user.read`, `user.write`, `user.assign_role`
- `rbac.elevate` (assume DPO/supervisor role)

Risk class: org-admin → A, role grants → B, list/read → C.

### Tests
- `tests/multi_tenant_isolation.rs` — 8 testów (addon A w org X nie widzi vector namespace addon A w org Y; cameras/policy claims org-scoped).
- `tests/rbac_permission_matrix.rs` — sweep każda rola × każda permission, 50+ assertions.
- `tests/migrations_v32_backfill.rs` — przed/po snapshot.
- `tentaflow-cli/tests/cli_org.rs` — round-trip create/invite/assign/list/remove.

### Risk register
| Ryzyko | P | Wpływ | Mitygacja |
|--------|---|-------|-----------|
| Backfill na audit_log (1M+ rows) blokuje startup | Med | Wysoki | Batche po 10k, progress log |
| Per-org addon install path migration psuje istniejące installs | Średnie | Wysoki | Symlink przejściowy `addons/<id>` → `orgs/org-default/addons/<id>` na czas migracji |
| Mesh HMAC keys per-node a nie per-org | Wysokie | Średni | Klucze zostają per-node; org_id w pickup-token claim weryfikuje czy token org_id == request org_id |
| Frontend musi zmienić wszystkie binary protocol calls | Wysokie | Średni | Session carries org_id, jedno-miejscowo injected w `api/binary-client.js` |

### Acceptance criteria
- [ ] Migracja v32 z `org-default` przechodzi na świeżej DB i F1c snapshot
- [ ] Wszystkie testy `multi_tenant_*` zielone
- [ ] `cargo build --features dashboard-api,camera` clean, clippy clean
- [ ] Codex review per chunk: P1.a (schema + repo), P1.b (RBAC middleware), P1.c (CLI + UI)
- [ ] Manual smoke: 2 orgs, każda z addonem, isolation potwierdzone

---

## Phase P2 — M16 v2 + service_list + round_robin

### What lands
- Host fn `service_list_v1(filter?) → Vec<ServiceInfo>` (`src/addon/host_functions/services.rs`), permission `service.read`, risk C, org-scoped.
- Host fn `node_resources_get_v1(node_id) → NodeResources`, permission `service.read`.
- Router `round_robin` strategy w `src/routing/middleware.rs`: per-alias atomic counter, modulo na active targets.
- M16 frontend: `tf-searchbox` + autocomplete źródło `service_list_v1`. Strategy radio dodaje option `round_robin`.
- SDK wrappers.

### Files / LOC
| File | LOC |
|------|-----|
| `src/addon/host_functions/services.rs` (new) | ~280 |
| `src/routing/middleware.rs` (+round_robin) | ~80 |
| `addon-sdk/sdk/src/lib.rs` (+wrappers) | ~120 |
| `www/js/services/aliases.js` (refactor) | ~150 |
| Tests | ~250 |
| Cleanup | -30 |

### DB schema changes (v33)
`ALTER TABLE model_aliases ... CHECK(strategy IN ('first_available','round_robin'))` (SQLite wymaga rebuild table).

### Tests
- 6 host fn unit (empty/filter/permission/org-scoping/node_resources mapping).
- 5 router round_robin (cycling/skip unhealthy/reset/fairness/concurrency).
- 3 E2E (Playwright): dropdown wyświetla, autocomplete, save persistuje.

### Acceptance criteria
- [ ] M16 dropdown live, autocomplete filtruje
- [ ] Round-robin 3 targety: 30 calls → 10/10/10 ±1
- [ ] Codex review chunk P2.a (host fn + SDK), P2.b (router + UI)

---

## Phase P3 — gate_check_cache + concurrency cap configurable

### What lands
- In-memory cache (DashMap) w `services::policy::engine::verify_claim`: lookup → hit (TTL 60s) → return; miss → eval → insert. Invalidation: `revoke_claim` flushuje claim_id; org config rotation flushuje all.
- Tabela `gate_check_cache` (reserved — w F2 in-memory primary, F3 włączy persistence).
- Manifest `[runtime]` rozszerzenie: `max_concurrency: u32`, `rate_limit_per_min: u32`. Admin override per addon install.

### Files / LOC
| File | LOC |
|------|-----|
| `src/services/policy/cache.rs` (new) | ~180 |
| `src/services/policy/engine.rs` (cache integration) | +40 |
| `src/addon/manifest.rs` (`[runtime]` fields) | +60 |
| `src/flow_runtime/scheduler.rs` (read manifest max_concurrency) | +50 |
| `src/services/service_call_rate_limit.rs` (per-addon override) | +40 |
| Admin UI override controls | ~80 |
| Tests | ~150 |

### DB schema (v34)
```sql
CREATE TABLE gate_check_cache (
    claim_id TEXT NOT NULL,
    ctx_hash TEXT NOT NULL,
    result TEXT NOT NULL,
    cached_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    PRIMARY KEY (claim_id, ctx_hash)
);
```

### Tests
- 8 unit cache (hit/miss/expiry/revoke invalidation/org scoping/no-poison).
- 3 bench (≥50x speedup hot path).
- 2 manifest parse.

### Risk register
- Cache poison przez błąd ctx_hash → hash MUSI zawierać org_id+addon_id+gate_id+resource_scope; cross-org isolation test.
- Memory growth → LRU cap 10k entries, eviction LRU.

### Acceptance criteria
- [ ] Hot path gate_check ≤ 50µs
- [ ] Cross-org cache isolation green
- [ ] Manifest override działa

---

## Phase P4 — Real camera Source operator

### What lands
- Per-camera `tokio::sync::broadcast::Sender<RawFrameRef>` w `services::camera_ingest` (capacity 100).
- `flow_runtime::operators::source::CameraSource` (replace current rejection): subscribe na broadcast, emit `Frame{camera_id, ts, raw_ref}`. Honor `fps` rate-limit.
- Backpressure: `recv()` lagging → audit `result='backpressure_drop'` collapsed.
- Org-scoping: addon w org X może subscribe tylko cameras org X.

### Files / LOC
| File | LOC |
|------|-----|
| `src/services/camera_ingest/broadcast.rs` (new) | ~200 |
| `src/services/camera_ingest/{mod,rtsp,onvif}.rs` (broadcast emit) | +120 |
| `src/flow_runtime/operators/source.rs` (rewrite — remove rejection) | ~350 |
| `src/flow_runtime/scheduler.rs` (camera source registration) | +60 |
| Tests | ~250 |
| Cleanup | -40 |

### Tests
- 10 unit (subscribe/fps rate-limit/backpressure/org isolation/multi-subscriber/cancellation cleanup).
- 1 integration: fake RTSP from file → flow `Source>Predict(echo)>Sink` przeleci 100 klatek bez panic, drop ≤ 5%.

### Risk register
- Lagged receiver wpływa na innych → `tokio::sync::broadcast` izoluje.
- Camera unsubscribe na flow stop → `cancel_on_drop` z F1c P5.

### Acceptance criteria
- [ ] Live RTSP test (mock z file) 100 klatek w < 5s
- [ ] Drop rate < 5% przy 30fps → 5fps konsument
- [ ] Org isolation: cross-org subscribe = `PermissionDenied`

---

## Phase P5 — Mesh broadcast-on-rotate

### What lands
- `services::mesh_keys::MeshKeyPool` event hook `on_local_key_rotation(name)` → broadcast `MESH_MSG_HMAC_KEYS_SYNC` natychmiast.
- Rate-limit: 1 broadcast/name/5s.
- Audit: `action='mesh.keys.broadcast_on_rotate'`, risk B.

### Files / LOC
| File | LOC |
|------|-----|
| `src/services/mesh_keys.rs` | +180 |
| `src/services/mesh/keys_sync.rs` | +80 |
| `tentaflow-cli/src/commands/keys.rs` | +30 |
| Tests | ~60 |

### Tests
- 3 unit (rate-limit/no-peers/partial fail audit).
- 1 integration: 2-node mesh, rotate na A, B widzi w < 2s.

### Acceptance criteria
- [ ] Rotation propaguje w ≤ 2s w 3-node lab
- [ ] Codex review chunk P5

---

## Phase P6 — ONVIF Profile M (analytics metadata)

### What lands
- ONVIF Media2 `GetMetadataConfigurations` + `GetMetadataConfigurationOptions`.
- PullPoint subscription (`CreatePullPointSubscription` + `PullMessages`) polling 1s.
- Metadata XML parser (events `tt:VideoAnalytics`, `tt:Object`, `tt:BoundingBox`) → `MetadataFrame`.
- Host fn `camera_metadata_subscribe_v1(camera_id) → MetadataStream`.
- Manifest `[[metadata_consumer]] camera_alias = "...", classes = ["..."]`.

### Files / LOC
| File | LOC |
|------|-----|
| `src/services/onvif/media.rs` (extend) | +180 |
| `src/services/onvif/events.rs` (new) | ~260 |
| `src/services/onvif/metadata_parser.rs` (new) | ~180 |
| `src/addon/host_functions/camera_metadata.rs` (new) | ~180 |
| SDK wrappers | +50 |
| Tests + fixtures | ~250 |

### DB (v35)
`ALTER TABLE cameras ADD COLUMN metadata_supported INTEGER DEFAULT 0`.

### Permission
- `camera.metadata` (risk C).

### Tests
- 8 unit (parser na XML fixtures z 3 vendorów, robust to malformed, classes filter).
- 3 integration (mock ONVIF PullPoint).

### Acceptance criteria
- [ ] 3 vendor fixtures parsują
- [ ] Mock test: addon subskrybuje → ≥5 events w 10s
- [ ] `camera.test_connection` wykrywa `metadata_supported`

---

## Phase P7 — Polished install wizard step + M15 panel

### What lands
- Frontend M15 panel "Discovered cameras" w `install-wizard.js`.
- tf-table: vendor, model, IP, MAC, "Add" button.
- Per-row tf-window: RTSP URI z `onvif_get_stream_uri_v1`, username, password, Test connection, Save → `camera_add_v1`.
- Refresh + Skip buttons.

### Files / LOC
| File | LOC |
|------|-----|
| `www/js/admin/install-wizard.js` | +250 |
| `www/templates/wizard-step-cameras.html` | ~80 |
| `www/css/install-wizard.css` | +50 |
| E2E Playwright | ~180 |

### Tests
- 5 E2E (discovery happy/refresh/add 1/add multiple/skip).
- 2 unit (form validation, test_connection error display).

### Acceptance criteria
- [ ] Manual test: świeży install + lab ONVIF kamera → wizard kończy z kamerą w `cameras`
- [ ] Codex review chunk P7

---

## Phase P8 — D4 RODO klauzula informacyjna — M10 generator

### What lands
- Templates Handlebars (3 warianty: short/standard/full).
- Generator `src/services/legal/rodo_generator.rs`: org config + admin form → Markdown → PDF (`genpdf`).
- Publikacja: `<tentaflow_home>/orgs/<org_id>/legal/rodo_<lang>_<version>.pdf` + signed URL.
- Admin UI w M10: tf-tabs "Templates / Versions / Publish".
- CLI: `tentaflow-cli legal rodo generate --org <id> --variant standard --lang pl`.

### Files / LOC
| File | LOC |
|------|-----|
| `src/services/legal/{mod,rodo_generator}.rs` | ~280 |
| `templates/rodo/*.hbs` | ~150 |
| `www/js/admin/compliance-rodo.js` + HTML | ~180 |
| `tentaflow-cli/src/commands/legal.rs` | ~120 |
| Tests | ~150 |

### DB (v36)
```sql
CREATE TABLE legal_documents (
    doc_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id),
    kind TEXT NOT NULL,
    lang TEXT NOT NULL,
    variant TEXT NOT NULL,
    version INTEGER NOT NULL,
    file_path TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    generated_at TEXT NOT NULL,
    generated_by_user TEXT NOT NULL
);
```

### Permissions
- `legal.write` (admin, risk A), `legal.read` (risk C).

### Tests
- 5 unit (template render 3×2, missing org config error, PDF byte sanity).
- 2 integration (generate + signed URL retrieve).

### Risk
- `genpdf` polskie znaki — bundle Liberation font, test PL chars day 1.

### Acceptance criteria
- [ ] Generate dla 1 org → PDF readable
- [ ] PL chars renderują się poprawnie
- [ ] Signed URL retrieval 200 OK

---

## Cross-phase concerns

### Migration ordering
F1c kończy na v31. F2 zajmie v32-v36:
- v32 (P1): organizations + roles + memberships + org_id columns + backfill
- v33 (P2): model_aliases strategy CHECK extension
- v34 (P3): gate_check_cache table (reserved)
- v35 (P6): cameras.metadata_supported
- v36 (P8): legal_documents

**Critical**: v32 najcięższe. Wszystkie kolejne zakładają obecność `org_id`. Niemożna zmienić kolejności.

### Backwards compatibility
- **Łamiemy**: single-tenant assumption. `org_id` NOT NULL po backfill. Stare addony bez org-aware permissions → domyślnie membership w `org-default`.
- **Łamiemy**: addon install path `addons/<id>/` → `orgs/<org>/addons/<id>/`.
- **Utrzymujemy**: binary protocol message kinds (dodajemy pola, nie struktur). Manifest TOML — dodajemy opcjonalne sekcje, istniejące bez zmian.

### Deferred to F3
- **ONVIF Profile G** (recording/search forensics) — zazębia się z F3 "API-8 recording full".
- **Distributed (cross-mesh) rate-limit** dla `service_call`.
- **Cross-addon FrameRef scoped sharing** (plan §16 open Q1).
- **WORM audit externalization format** (plan §16 open Q4).
- **D1 ADR Flow end-to-end** z 4 inference blocks + services yolo/ocr w Dockerze — F2 daje engine, F3 deploys.
- **PullPoint → PushPoint WebSocket** dla ONVIF metadata.
- **Frame proxy chunked transport** dla > 16 MiB (4K RGB24).
- **Signed PDF** dla RODO (Ed25519 + timestamp authority).

## Total estimate

- **LOC**: ~6750
- **Phases**: 8
- **Codex review checkpoints**: 14 (per phase + per fix-up)
- **Duration**: 6-8 tygodni single dev / 4-5 tygodni team 2

**Gantt-style**:
- Tydzień 1-2: P1 (blocker)
- Tydzień 3: P2 + P3 (równolegle)
- Tydzień 4: P4
- Tydzień 5: P6 + P5 (P5 w tle)
- Tydzień 6: P7 + P8 równolegle
- Tydzień 7-8: integration soak, RELEASE-F2.md

## Następny krok PM

Zatwierdź 5 decyzji architektonicznych w sekcji "Decyzje PM wymagane" lub zmień. Po zatwierdzeniu — delegacja P1 do `programista-rust` (chunk P1.a — schema + repo).
