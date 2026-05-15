# TentaVision F1a — implementation plan (week-by-week)

**Wersja:** v0.3.2 · M1.W6 ukończony (Chunks A/B/C/D) — cameras table v21 + GStreamer FakeFile supervisor + 10 host functions ABI + e2e WASM addon + security suite. Wszystkie codex review fixes applied.

**Poprzednia:** v0.3.1 · Chunk C ukończony — system uprawnień dwukierunkowych zaimplementowany, runtime alias CRUD ABI usunięte.

**Wcześniejsza:** v0.3 · rewizja M1.W5 — runtime alias CRUD usuniety, dodany system dwukierunkowych uprawnien (visibility + consumers + uses_*); Chunk C kompletnie przepisany; Chunk D (admin UI) wydzielony i przesuniety do M2.

**Wcześniejsza:** v0.2 · po pytaniu usera "dlaczego stub?" — usunięte stuby host functions, każdy tydzień M1 dostarcza PEŁNĄ implementację jednej kategorii API (SQL/Alias/Camera/Streaming/Recording). Boilerplate (AbiError, sdk_version, payload limits) skondensowany w M0.W2. Brak fazy "scaffolding stubs → później pełna implementacja" — zgodnie z project rules CLAUDE.md "no stubs/placeholders/TODO".
**Cel:** wykonalny tygodniowy plan implementacji fazy F1a (10–16 tygodni jednego seniora / 6–8 tygodni 2-osobowego zespołu)
**Powiązane:**
- `tentavision-plan.md` (v0.5.3) — pełna specyfikacja, ABI, schema, mockupy, decyzje techniczne
- `tentavision-sdk-research.md` — analiza istniejącego SDK z cytatami kodu
- Mockupy: `~/.gstack/projects/Slyb00ts-TentaFlow/designs/tentavision-v1/`

---

## Spis treści

1. Cel F1a i Definition of Done
2. Pre-requisites (co musi być przed startem)
3. Dependency graph
4. Milestone overview
5. Milestone M0 — Foundation (tyg. 1–3) — manifest + SDK boilerplate + CLI/DB migrations
6. Milestone M1 — Backend host functions PEŁNE (tyg. 4–8)
7. Milestone M2 — UI M14/M15/M16 + integration tests (tyg. 9–11)
8. Milestone M3 — Acceptance: UI e2e + perf + soak + release (tyg. 12–15)
9. Risk register
10. Test execution plan
11. Tooling & infrastructure setup
12. Communication cadence
13. F1a → F1b handoff plan
14. Co celowo poza F1a (deferred do F1b/F1c/F2/F3/F8/F10)

---

## 1. Cel F1a i Definition of Done

### 1.1 Cel jednoznaczny

Po F1a addon TentaVision (w wersji szkieletowej, **bez** D1/D2/D3/D5/D6 logic) musi się dać:
1. Zainstalować w TentaFlow przez wizard M15 (krok 1-3, kroki 4-6 placeholder)
2. Utworzyć 6 aliasów AI w globalnej tabeli `model_aliases`
3. Pokazać readonly M14 z 6 aliasami i statystykami storage (KV/SQL/Vector*/Recording*)
4. Admin może wejść na `/services/aliases` (M16 v1) i edytować target_model każdego aliasu (text input)
5. Addon może wywołać `service_call("tentavision-yolo", "detect", payload{raw_ref})` na fake mock-service zwracającym pusty payload (sanity check ścieżki)
6. Addon może subskrybować strumień z FakeFile camera (mp4 replay), dostać RawFrameRef i przekazać przy service_call
7. Addon może zapisać snapshot i pobrać URL signed
8. Wszystkie operacje przechodzą przez audit log z risk_class
9. Test matrix (§17 planu v0.5.3) zielony

\* Vector i Recording w F1a tylko basic (minimal API) — pełna funkcjonalność w F2/F3.

### 1.2 Definition of Done (każde MUSI być zielone)

| # | Kryterium | Weryfikacja |
|---|-----------|-------------|
| DoD-1 | TentaVision addon installable przez M15 wizard | Manual test + automated install-uninstall-reinstall loop |
| DoD-2 | 6 aliasów w `model_aliases` po install, owner=tentavision | SQL query po install |
| DoD-3 | M14 renderuje 6 aliasów + 4 storage cards | UI e2e test (§17.6 #M14) |
| DoD-4 | M16 v1 admin może edit target_model + fallback_targets + strategy | UI e2e test (§17.6 #M16) |
| DoD-5 | `service_call(alias, method, payload)` działa end-to-end z mock service | Integration test |
| DoD-6 | FakeFile camera → stream_subscribe → stream_next → service_call → pickup_frame z mock service | E2E test z `sample_traffic.mp4` |
| DoD-7 | `recording_save_snapshot` + `recording_get_url` działa | Integration test |
| DoD-8 | Per-addon SQLite file utworzony w `~/.tentaflow/addons/tentavision/data.db`, schema z migrations applied | FS check + SQL query |
| DoD-9 | Permission denied zwraca ABI_ERR_PERMISSION + audit anomaly | Security test |
| DoD-10 | PickupToken replay → 403 + audit anomaly | Security test (§17.5) |
| DoD-11 | Path traversal w camera_add → blocked | Security test |
| DoD-12 | SQL injection guard działa (params bind) | Security test |
| DoD-13 | Performance: service_call overhead < 5ms p99, stream_next < 1ms p99, sql_exec < 5ms p99 | Bench (§17.8) |
| DoD-14 | Wszystkie 24 error codes correctly returned w odpowiednich scenariuszach | Comprehensive test sweep |
| DoD-15 | Audit chain verify przechodzi (Merkle hash chain z genesis) | Unit test |
| DoD-16 | Migration apply idempotent: install→uninstall→install nie psuje DB | Integration test |
| DoD-17 | F1a release notes + migration guide dla teams-bot addon | Doc artifact |

### 1.3 Czego F1a NIE robi

Świadomie poza scope F1a:
- D1-D6 logic (modele inferencji są mock w F1a)
- Real RTSP/ONVIF (FakeFile only)
- Custom UI components z signature (F1c)
- Policy/claims engine (F2)
- Vector store full (F2 — w F1a tylko API stub że call zwraca empty)
- Full Recording API (ring-buffer, retention) — F3
- Evidence signing (F3)
- Flow invoke (F2)
- PostgreSQL (F8)

---

## 2. Pre-requisites (co musi być przed startem)

### 2.1 Decyzje już podjęte (w planie v0.5.3)

- ✅ Camera ingest backend: **GStreamer** (§16.1)
- ✅ Aliasy: `model_aliases` (z fallback chain), nie `service_aliases` (§16.2)
- ✅ Permission naming: `secrets.*`, `events.*`, kropka separator (§16.3)
- ✅ FrameRef lifecycle: RawFrameRef + PickupToken + frame_url (§16.4, §6.4)
- ✅ SQL backend default: SQLite per-addon (F1a), PostgreSQL F8 (§16.5)
- ✅ Strategy: tylko `first_available` w MVP (§16.6)
- ✅ UI components: Ed25519 + iframe sandbox (F1c, nie F1a)

### 2.2 Środowisko developerskie

| Wymagane | Wersja | Notatka |
|----------|--------|---------|
| Rust | 1.85+ | Edition 2024 |
| wasmtime | latest stable | dla addon runtime |
| wasm32-wasip1 target | — | `rustup target add wasm32-wasip1` |
| wasm-bindgen-cli | 0.2.108+ | dla protocol-wasm glue |
| sqlite3 | 3.40+ | CLI debug |
| GStreamer | 1.22+ | gst-libav, gst-plugins-good, gst-plugins-bad, gst-plugins-ugly |
| Node.js | 20+ | tylko jeśli development www/ frontend |
| Docker | latest | mock services |
| `cargo install criterion` | latest | benchmarks |

### 2.3 Pliki / katalogi przygotowane przed M0

- `tentaflow-core/addons-pro/tentavision/` — szkielet TentaVision addon source (utworzony jako część M0.W1)
- `tentaflow-core/src/addon/host_functions/` — istnieje, dodajemy nowe pliki
- `tentaflow-core/src/services/` — istnieje, dodajemy nowe moduły
- `~/.tentaflow/addons/` — runtime tworzy per-addon FS sandbox
- `assets/test/sample_traffic.mp4` — testowe wideo (5 min, ruch, ciężarówki) — przygotowane przed M2

### 2.4 Mock services Docker images

Przygotowane przed M2 jako simple Python/Rust HTTP servers:
- `mock-yolo-detector` — przyjmuje POST z frame ref → zwraca losowe bboxy
- `mock-ppocrv5-ocr` — zwraca przykładowy tekst
- `mock-siglip-vlm` — zwraca losowy wektor 768D
- `mock-tts/stt/llm` — istniejące mock-i z teams-bot test setup (reusowane)

---

## 3. Dependency graph

```
M0 Foundation (tyg. 1-3, manifest + boilerplate + DB migrations)
  │
  ▼
M1 Backend host functions PEŁNE (tyg. 4-8)
  W4: SQL → W5: Alias → W6: Camera → W7: Streaming → W8: Recording
  │   każdy tydzień produkcyjna implementacja jednej kategorii + docs
  ▼
M2 UI M14/M15/M16 + integration tests + security tests (tyg. 9-11)
  │
  ▼
M3 UI e2e + perf + 24h soak + acceptance (tyg. 12-15)
  │
  ▼
F1a DONE → F1b kickoff
```

**Critical path:** M0 → M1 → M2 → M3 (15 tyg. linear)
**Parallel opportunities:** W M1 — agent UI może już zacząć frontend M16 (HTML structure z mockupu) gdy backend SQL/Alias są jeszcze w toku (od tyg. 6). Realny crash to 12-13 tyg. dla 2-os zespołu.

---

## 4. Milestone overview (v0.2 — bez stubów)

| Milestone | Tygodnie | Główny deliverable | Demo punkt |
|-----------|----------|---------------------|------------|
| **M0** Foundation | 1–3 | Manifest parser z 7 nowymi sekcjami (W1 ✅), SDK boilerplate (AbiError, sdk_version, payload limits, audit risk_class) (W2), CLI validate + DB migrations (W3) | `cargo test` zielony, CLI waliduje TentaVision manifest, DB ma nowe tabele |
| **M1** Backend host functions (PEŁNE, bez stubów) | 4–8 | M1.W4 SQL host functions + per-addon SQLite + migrations runner; M1.W5 Alias CRUD + M16 backend; M1.W6 Camera FakeFile + GStreamer; M1.W7 Streaming + RawFrameRef + PickupToken; M1.W8 Recording basic + frame_url | Addon TentaVision e2e: install, alias create, FakeFile camera, frame ref, mock service call, snapshot URL |
| **M2** UI (M14/M15/M16) | 9–11 | Frontend M14 readonly + M15 wizard kroki 1-3 + M16 v1 admin UI, integration tests, security tests | Admin install z marketplace, wizard, M14 widzi aliasy, M16 edytuje |
| **M3** Acceptance + perf | 12–15 | UI e2e (Playwright), performance benchmarks, 24h soak, release notes, teams-bot migration | F1a release tag, demo dla stakeholderów |
| **M4 (zlikwidowane)** | — | Zakres przeniesiony do M3 (acceptance) i części M2 (UI). Total 15 tyg. zamiast 16 — usunięcie fazy stubs odzyskało tydzień |

---

## 5. Milestone M0 — Setup + ABI scaffolding (tyg. 1–3)

### M0.W1 — Manifest parser + new sections

**Scope:**
- Rozszerzenie `tentaflow-core/src/addon/lifecycle.rs` parsera manifestu o nowe sekcje:
  - `[storage]` z polami `kv`, `sql`, `sql_backends`, `sql_dialect`, `migrations_dir`, `encryption`
  - `[[alias]]` z `id`, `display_name`, `methods`, `suggested_default`, `gate`
  - `[[gate]]` z `id`, `display_name`, `required_claims` (stub — pełna interpretacja w F2)
  - `[[vector_namespace]]` (stub)
  - `[[flow_template]]` (stub)
  - `[[ui_component]]` (stub — pełna w F1c)
  - `[gpu]` (informational)
- Walidacja: missing required fields, conflicts (alias id collision), invalid enums
- Unit tests dla każdej sekcji (parse OK, parse fail z proper error)

**Files touched:**
- `tentaflow-core/src/addon/lifecycle.rs` (rozszerzenie `parse_manifest`)
- `tentaflow-core/src/addon/manifest.rs` (nowe structures: `AliasSpec`, `StorageConfig`, `GateSpec`, ...)
- `tentaflow-core/tests/addon_manifest_parsing.rs` (nowy)

**Acceptance:**
- `cargo test addon_manifest_parsing` passes
- Manifest TentaVision (z `tentavision-plan.md` §5) parsuje bez błędów

**Tygodniowy demo:** parse manifest TentaVision → printout struktur w terminal (CLI tool `tentaflow-cli addon parse manifest.toml`)

### M0.W2 — SDK boilerplate (PEŁNE prerequisite dla wszystkich host functions)

**Bez stubów. To wspólne primitive-y używane przez każdą host function w M1.**

**Scope:**
- `tentaflow-core/src/addon/errors.rs` (NEW) — `AbiError` enum z 24 kodami z planu v0.5.3 §6.2.Y (ABI_OK=0, ABI_ERR_PERMISSION=1, ..., ABI_ERR_FRAME_PURGED=24). Konwencja: `impl From<AbiError> for i32`, helper `bail!(err)` macro
- `tentaflow-core/src/addon/sdk_version.rs` (NEW) — `CORE_SDK_VERSION: semver::Version`, funkcja `check_addon_sdk_compatibility(manifest.sdk_version, CORE_SDK_VERSION) -> Result<(), AbiError>` wywoływana w `lifecycle.rs::install_addon` przed załadowaniem WASM
- `tentaflow-core/src/addon/host_functions/abi_helpers.rs` (NEW) — wspólne helpery:
  - `enforce_payload_size(len: usize, kind: PayloadKind) -> Result<(), AbiError>` z konfiguracją max per kind (service_call=8MB, sql=4MB, vector_item=1MB, ui_render=2MB, secret=64KB)
  - `write_output_with_retry_semantics(actual_data: &[u8], out_ptr, out_cap, out_len_ptr) -> i32` — implementuje out_cap retry pattern z §6.2.Y (jeśli out_cap < actual.len → zapisz wymagany rozmiar do out_len_ptr i zwróć ABI_ERR_OUTPUT_BUFFER_TOO_SMALL)
- `tentaflow-core/src/audit/mod.rs` (extend) — rozszerzenie `audit_log` funkcji o parametr `risk_class: RiskClass`, `related_claim_id: Option<String>`, `request_id: Option<String>`. RiskClass enum z A/B/C/Unclassified
- `tentaflow-core/src/addon/host_functions/mod.rs` (extend) — `audit_log_with_risk(state, action, resource_type, resource_id, risk_class, related_claim_id, result, error_message)` jako wrapper

**Files touched:**
- `tentaflow-core/src/addon/errors.rs` (NEW, ~100 linii)
- `tentaflow-core/src/addon/sdk_version.rs` (NEW, ~50 linii)
- `tentaflow-core/src/addon/host_functions/abi_helpers.rs` (NEW, ~150 linii)
- `tentaflow-core/src/addon/host_functions/mod.rs` (extension)
- `tentaflow-core/src/addon/lifecycle.rs` (wpięcie sdk_version check)
- `tentaflow-core/src/audit/mod.rs` (rozszerzenie audit_log signatury)
- `tentaflow-core/src/db/migrations.rs` (ALTER audit_log + risk_class column, related_claim_id, request_id — przesunięte tu z M0.W3 bo audit_log_with_risk tego potrzebuje)
- `tentaflow-core/tests/sdk_boilerplate.rs` (NEW)
- `tentaflow-core/docs/ADDON_HOST_FUNCTIONS.md` (UPDATE) — pełna sekcja "Globalne kody błędów" z 24 kodami + sekcja "Konwencje ABI" rozszerzona o payload limits i out_cap retry pattern

**Acceptance:**
- `cargo test sdk_boilerplate` zielony
- AbiError → i32 mapping (każdy z 24 kodów zwrócony i sprawdzony)
- `enforce_payload_size(9_000_000, PayloadKind::ServiceCall)` → `Err(AbiError::PayloadTooLarge)`
- `write_output_with_retry_semantics` z out_cap=10 i actual=100 bajtów → zwraca ABI_ERR_OUTPUT_BUFFER_TOO_SMALL i zapisuje 100 do out_len_ptr
- `check_addon_sdk_compatibility(VersionReq::parse(">=2.0.0").unwrap(), Version::parse("1.0.0").unwrap())` → `Err(AbiError::SdkVersionMismatch)`
- Audit log z risk_class="C" zapisany do DB, kolumna `audit_log.risk_class` istnieje
- TentaVision manifest z `sdk_version = ">=0.2.0"` installuje gdy core SDK = 0.2.0; rejected gdy core SDK = 0.1.0

**Demo:** Cargo test integracja: instaluje TentaVision z sdk_version >=99.0.0 → rejected z czytelnym error message. Plus istniejący test-app i teams-bot nadal installują (sdk_version optional = always pass).

### M0.W3 — CLI tool + DB migrations finalne

**Scope:**
- `tentaflow-cli/src/commands/addon.rs` (NEW lub EXTEND) — komenda `tentaflow-cli addon validate <path-to-addon-dir>`:
  - Wczytuje `manifest.toml` z `path`
  - Parsuje przez `parse_manifest_toml` z `lifecycle.rs` (z M0.W1)
  - Wywołuje `validate_manifest_extensions` (z M0.W1)
  - Sprawdza obecność plików referowanych: `wasm_file`, `migrations_dir/*.sql`, `flow_template.path`, `ui_component.src`
  - Sprawdza signature format Ed25519 (regex z M0.W1)
  - Sprawdza sdk_version compat przeciwko core (z M0.W2)
  - Wypisuje: lista permissions, aliases, network rules, gates, validation result (OK / errors lista)
- Migracje DB w `tentaflow-core/src/db/migrations.rs` (NEW migration files lub w istniejącym):
  - `model_alias_owners(alias_id, owner_type, owner_id, created_at)` z planu §6.5
  - `alias_calls(id, alias_id, alias_name, method, target_used, target_node_id, service_id, caller_addon_id, caller_user_id, request_id, duration_ms, payload_bytes, response_bytes, fallback_used, fallback_chain_position, result, error_code, ts)` — pełna definicja z planu §6.5
  - `model_alias_changes(id, alias_id, alias_name, changed_by_user_id, changed_by_addon_id, before_snapshot, after_snapshot, change_type, reason, ts)`
  - `addon_migrations_applied(addon_id, migration_name, migration_hash, applied_at, applied_in_addon_version, status, error_message, duration_ms)`
  - `frame_pickup_log(id, raw_frame_ref, service_id, caller_addon_id, request_id, picked_up_at, result)`
  - Wszystkie indeksy z planu §6.5

**Files touched:**
- `tentaflow-cli/src/commands/addon.rs` (NEW lub extension)
- `tentaflow-cli/src/main.rs` (rejestracja komendy)
- `tentaflow-core/src/db/migrations.rs` (5 nowych migracji)
- `tentaflow-core/tests/db_migrations.rs` (NEW) — verify że migracje apply idempotent, każda nowa tabela ma indeksy
- `tentaflow-core/tests/cli_addon_validate.rs` (NEW)

**Acceptance:**
- `tentaflow-cli addon validate /home/critix/repos/rust/TentaFlow/tentaflow-core/addons/test-app-addon` → OK
- `tentaflow-cli addon validate /tmp/broken-manifest/` (z duplicate alias id) → wypisuje error z linia/kolumna/details
- Re-run migracji idempotent (drugi run nie tworzy duplikatów ani błędów)
- Nowe tabele istnieją po fresh DB init: `model_alias_owners`, `alias_calls`, `model_alias_changes`, `addon_migrations_applied`, `frame_pickup_log`
- TEAMS_BOT_ALIASES nadal w `model_aliases` (nieusuwane w M0; ich migracja do nowego `[[alias]]` manifestu jest w M1.W5)

**Demo:** koniec M0 — wszystkie 3 nowe komendy CLI działają, `cargo test --workspace` zielony.

**M0 acceptance gate:**
- DoD-14 (error codes work) ✓ — wszystkie 24 kody testowane
- DoD-16 (migrations idempotent) ✓ — częściowo (pełne testowane w M1.W4 z addon migrations runner)
- Unit test coverage > 70% dla nowego kodu w `errors.rs`, `sdk_version.rs`, `abi_helpers.rs`
- `tentaflow-cli addon validate` testuje na 5 manifestach: test-app, teams-bot, TentaVision (planowany), broken-1 (missing field), broken-2 (duplicate alias)

---


## 6. Milestone M1 — Backend host functions PEŁNE (tyg. 4–8)

Każdy tydzień M1 = jeden production-ready komponent. Zero stubów. Każda host function zaimplementowana wraz z testami integration + security + dokumentacją dev.

### M1.W4 — SQL host functions PEŁNE + per-addon SQLite + migrations runner

**Scope:**
- Per-addon FS sandbox `tentaflow-core/src/addon/fs_sandbox.rs` (path sanitization, idempotent setup)
- Per-addon SQLite `tentaflow-core/src/addon/storage_sql.rs` (r2d2_sqlite pool, WAL mode, foreign_keys=ON, lazy init)
- Migrations runner `tentaflow-core/src/addon/migrations.rs` (apply leksykograficznie, atomic per migration, SHA256 hash verification, idempotent re-install, wpisy do `addon_migrations_applied`)
- Host functions PEŁNE w `host_functions/sql.rs`:
  - `sql_exec_v1`, `sql_query_v1`, `sql_query_one_v1`, `sql_transaction_v1`
  - Parametryzacja przez `?` (rusqlite bind), nigdy string concat
  - DDL block at runtime → `AbiError::Permission`
  - Query timeout 30s, payload size enforce ≤ 4MB
- `addon-sdk/sdk/src/lib.rs` bindings + high-level wrappers

**Files:** `fs_sandbox.rs` (~150L), `storage_sql.rs` (~200L), `migrations.rs` (~250L), `host_functions/sql.rs` (~400L), 4 test files, `docs/ADDON_HOST_FUNCTIONS.md` sekcja 11 SQL API.

**Acceptance:** test addon `sql-test-addon` wykonuje pełne CRUD — zielony. DDL → permission denied. SQL injection przez bind param → escaped. Migrations idempotent. INSERT < 5ms p99.

### M1.W5 — Alias lifecycle (install/uninstall) + readonly ABI + permission model + teams-bot migration

Tydzien rozbity na 4 chunki. Po feedbacku usera (v0.3) wycofany pierwotny
runtime alias CRUD ABI (`alias_create_v1` / `alias_deactivate_v1`); aliasy
tworzone i deaktywowane wylacznie przez lifecycle hooks core. Dodany pelny
system dwukierunkowych uprawnien (visibility + consumers + uses_*) — zob.
plan §6.6.

#### Chunk A — DB refactor + teams-bot migration  `[completed]`

**Scope:**
- Lifecycle hooki w `addon/lifecycle.rs`:
  - `install_manifest_aliases(addon_id, &manifest)` — czyta `[[alias]]`, zapisuje do `model_aliases` z owner = addon, plus `model_alias_owners`. Idempotent (reinstall = reactivate).
  - `deactivate_aliases_owned_by_addon(addon_id)` — `is_active=0` dla wszystkich aliasow z `owner_id = addon_id`.
- Rozszerzenie `repository::create_or_reactivate_model_alias` o owner_type/owner_id → `model_alias_owners`.
- **Teams-bot migration**:
  - `addons-pro/teams-bot/manifest.toml` dostaje `[[alias]]` (5 aliasow).
  - Usuniecie `TEAMS_BOT_ALIASES` + `activate_teams_aliases` + `deactivate_teams_aliases` z `addon/mod.rs:1880` (project rules: "no backward-compat shims").
  - One-shot migration script: istniejace wpisy → `model_alias_owners`.

#### Chunk B — Readonly alias host functions + SDK  `[completed] (po rollbacku create/deactivate)`

**Scope:**
- Host functions w `host_functions/aliases.rs`: `alias_get_v1`, `alias_list_owned_v1` (readonly). Stats stripowane gdy `caller != owner`.
- SDK wrappery w `addon-sdk`: `alias_get(id)`, `alias_list_owned()`.
- Permission `alias.read` (uprzednio `alias.manage`).

Notka: Chunk B pierwotnie zawieral takze `alias_create_v1` + `alias_deactivate_v1` ABI; po feedbacku usera cofniete do readonly-only — alias lifecycle wylacznie przez install/uninstall hooks.

#### Chunk C — Rollback runtime CRUD + dwukierunkowe uprawnienia (visibility + consumers + uses_*)  `[completed]`

**Status realizacji:**
- Migracje DB v14–v20 wdrożone: rename `alias.manage`→`alias.read` oraz tabele `model_alias_visibility`, `model_alias_consumers`, `model_visibility`, `model_consumers`, `addon_uses_alias`, `addon_uses_model` z indeksami.
- Manifest parser: dodany `AliasVisibility` enum + pola `visibility`/`allowed_consumers` w `[[alias]]`; nowe sekcje `[[uses_alias]]`/`[[uses_model]]` ze strukturami `UsesAliasSpec`/`UsesModelSpec`; walidacja kombinacji visibility×consumers oraz duplikatów id.
- `install_manifest_aliases` rozszerzony o UPSERT visibility/consumers + uses_* + reconciliation pending→granted/auto_granted/denied; całość w jednej zewnętrznej transakcji (atomowy install).
- Resolver `resolve_model_alias` przyjmuje `caller_addon_id: Option<&str>` i zwraca nowy wariant `AliasPermissionDenied`; istniejące callery przekazują `None` (zero behavior change); addon path do podłączenia w M1.W7.
- Rollback Kroku 1: usunięte `alias_create_v1`, `alias_deactivate_v1`, ich SDK wrappery, 19 testów ABI i 2 linker registracje (wasmtime).
- Permission rename `alias.manage` → `alias.read` zastosowany w manifest teams-bot, test fixtures oraz w `aliases.rs` (const).
- Bilans LOC: ~1300 dodanych / ~927 usuniętych netto. Testy zielone: 1255 lib + nowe testy reconcile/permission/migration.

**Scope:**

1. **Rollback ABI** `alias_create_v1` + `alias_deactivate_v1` z `host_functions/aliases.rs` oraz z linker registration (wasmtime `Linker::func_wrap`).
2. **Rollback SDK** wrapperow `alias_create()` + `alias_deactivate()` z addon-sdk (zostawiajac tylko readonly).
3. **Rollback testow** ABI dla create/deactivate (`tests/wasm_abi/aliases_*.rs`).
4. **Rename permission** `alias.manage` → `alias.read`:
   - DB migration #13: `UPDATE addon_permissions SET permission_id='alias.read' WHERE permission_id='alias.manage';` (idempotent).
   - Manifest teams-bot `manifest.toml`: replace `alias.manage` → `alias.read`.
5. **DB migracje #14-#19** (schema):

   ```sql
   -- #14 visibility per alias
   CREATE TABLE model_alias_visibility (
     alias_id INTEGER PRIMARY KEY REFERENCES model_aliases(id) ON DELETE CASCADE,
     visibility TEXT NOT NULL CHECK(visibility IN ('private','restricted','public')),
     created_at TEXT NOT NULL DEFAULT (datetime('now'))
   );

   -- #15 consumers per alias (granty)
   CREATE TABLE model_alias_consumers (
     alias_id INTEGER NOT NULL REFERENCES model_aliases(id) ON DELETE CASCADE,
     consumer_addon_id TEXT NOT NULL,
     granted_by_user_id INTEGER,
     status TEXT NOT NULL CHECK(status IN ('pending','granted','denied')),
     created_at TEXT NOT NULL DEFAULT (datetime('now')),
     PRIMARY KEY (alias_id, consumer_addon_id)
   );
   CREATE INDEX idx_alias_consumers_addon ON model_alias_consumers(consumer_addon_id, status);

   -- #16 visibility per model
   CREATE TABLE model_visibility (
     model_id TEXT PRIMARY KEY,        -- FK do rejestru modeli (system/manual)
     visibility TEXT NOT NULL CHECK(visibility IN ('restricted','public')),
     created_at TEXT NOT NULL DEFAULT (datetime('now'))
   );

   -- #17 consumers per model
   CREATE TABLE model_consumers (
     model_id TEXT NOT NULL,
     consumer_addon_id TEXT NOT NULL,
     granted_by_user_id INTEGER,
     status TEXT NOT NULL CHECK(status IN ('pending','granted','denied')),
     created_at TEXT NOT NULL DEFAULT (datetime('now')),
     PRIMARY KEY (model_id, consumer_addon_id)
   );
   CREATE INDEX idx_model_consumers_addon ON model_consumers(consumer_addon_id, status);

   -- #18 deklaracje uses_alias z manifestu
   CREATE TABLE addon_uses_alias (
     addon_id TEXT NOT NULL,
     alias_id INTEGER NOT NULL REFERENCES model_aliases(id) ON DELETE CASCADE,
     required INTEGER NOT NULL DEFAULT 0,
     reason TEXT,
     status TEXT NOT NULL CHECK(status IN ('pending','granted','denied')),
     created_at TEXT NOT NULL DEFAULT (datetime('now')),
     PRIMARY KEY (addon_id, alias_id)
   );
   CREATE INDEX idx_addon_uses_alias_status ON addon_uses_alias(addon_id, status);

   -- #19 deklaracje uses_model z manifestu
   CREATE TABLE addon_uses_model (
     addon_id TEXT NOT NULL,
     model_id TEXT NOT NULL,
     required INTEGER NOT NULL DEFAULT 0,
     reason TEXT,
     status TEXT NOT NULL CHECK(status IN ('pending','granted','denied')),
     created_at TEXT NOT NULL DEFAULT (datetime('now')),
     PRIMARY KEY (addon_id, model_id)
   );
   CREATE INDEX idx_addon_uses_model_status ON addon_uses_model(addon_id, status);
   ```

6. **Manifest parser**: rozszerzenie `AliasSpec` w `addon/manifest.rs` o pola:
   ```rust
   pub struct AliasSpec {
     // istniejace: id, display_name, methods, suggested_default, gate
     pub visibility: AliasVisibility,           // default Private
     pub allowed_consumers: Vec<String>,
   }
   pub enum AliasVisibility { Private, Restricted, Public }
   ```
   Dodanie nowych struktur:
   ```rust
   pub struct UsesAliasSpec { pub id: String, pub required: bool, pub reason: String }
   pub struct UsesModelSpec { pub id: String, pub required: bool, pub reason: String }
   ```
   Walidacja w `validate_manifest_extensions`:
   - `visibility="restricted"` ⇒ `allowed_consumers` niepuste; inaczej parser blad.
   - `visibility="private"` lub `"public"` ⇒ `allowed_consumers` musi byc pusty/nieobecny; inaczej parser blad.
   - Duplikaty `id` w `[[uses_alias]]` / `[[uses_model]]` = blad.

7. **Install hook** (rozszerzenie `install_manifest_aliases` + nowe funkcje):
   - Po zapisie aliasu do `model_aliases` + `model_alias_owners`:
     - INSERT do `model_alias_visibility` z `visibility` z manifestu.
     - Dla `restricted` — bulk INSERT do `model_alias_consumers` dla kazdego z `allowed_consumers` ze `status='granted'`.
   - Po przetworzeniu `[[uses_alias]]` / `[[uses_model]]`:
     - INSERT do `addon_uses_alias` / `addon_uses_model` ze `status` ustalonym przez reconcile:
       - Owner = caller? n/d (consumer != owner).
       - Visibility = `public` → `granted`.
       - Visibility = `restricted` i `caller_addon_id` w `model_alias_consumers` → `granted`.
       - Visibility = `restricted` bez wpisu → `pending`.
       - Visibility = `private` → `denied`.
     - Jesli ktorys `required=true` ma status != `granted` → install rejected (rollback transakcji), wizard pokazuje brakujace granty.

8. **Resolver aliasow** w `service_call_v1`:
   - Przed routingiem: `if caller_addon_id != owner_id { SELECT status FROM addon_uses_alias WHERE (addon_id, alias_id) AND status='granted'; }`. Brak rekordu → `ABI_ERR_PERMISSION`, audit `alias_calls.result='permission_denied'`.
   - Identycznie dla bezposredniej sciezki modelu (rzadkiej) — sprawdza `addon_uses_model`.

9. **Dokumentacja**: aktualizacja `docs/ADDON_MANIFEST.md` (sekcja `[[alias]]` o `visibility`/`allowed_consumers`; nowe sekcje `[[uses_alias]]`, `[[uses_model]]`) i `docs/ADDON_HOST_FUNCTIONS.md` (sekcja 12 readonly Aliases; usuniecie wzmianek o create/deactivate) — zob. odpowiednio.

**Files:** `host_functions/aliases.rs` (-150L po rollbacku, +50L permission check w resolver), `addon/manifest.rs` (+UsesAliasSpec/UsesModelSpec + AliasVisibility), `addon/lifecycle.rs` (rozszerzony install_manifest_aliases + reconcile), `db/migrations.rs` (+6 migracji #14-#19), `db/migrations/013_rename_alias_manage_to_read.sql`, 4 test files (manifest parsing, install reconcile, resolver permission, migration idempotency), updated docs.

**Acceptance:**
- Manifest parser: TentaVision install z `visibility="public"` dla 6 aliasow → 6 rekordow `model_alias_visibility`.
- Teams-bot install z `[[uses_alias]] id="tentavision-yolo" required=false reason="..."` → `addon_uses_alias` rekord, `status='granted'` bo alias public.
- Manifest `restricted` z pustym `allowed_consumers` → parser rejection.
- Manifest `public` z `allowed_consumers=["x"]` → parser rejection.
- Resolver: addon bez grantu probuje `service_call` → `ABI_ERR_PERMISSION` + audit `permission_denied`.
- Reinstall consumera nie duplikuje wpisow `addon_uses_*` (PK unique).
- `required=true` bez grantu → install rejected, czytelny komunikat w wizard.

#### Chunk D — Admin UI dla visibility/consumers  (przeniesione do M2)

Backend Chunka C wystawia operacje grantowania/odbierania przez istniejacy
WebTransport/WebSocket binary protocol — **nie przez REST**. UI to robota
M2 (M14b/M15b/M16b) i M8b dla modeli. Tutaj tylko wzmianka jako dependency.

Mockupy: `~/.gstack/projects/Slyb00ts-TentaFlow/designs/tentavision-v1/`
(M8b, M12b, M15b, M16b).

**Files (M2):** rozszerzenie `www/js/pages/services-aliases.js` (M16b consumers dialog), `www/js/pages/addons/install-wizard.js` (M15b krok 4 grants), `www/js/pages/models/registry.js` (M8b consumers dialog), `www/js/pages/addons/<addon>/permissions.js` (M12b lista uses_*).

**Acceptance (M2):** Admin moze w M16b/M8b przyznac grant `pending` → `granted` na restricted alias/model; addon ktory deklaruje `[[uses_alias]] required=true` z `pending` widzi install blokowany w M15b z czytelnym CTA do M16b.

### M1.W6 — Camera API PEŁNA + FakeFile connector + GStreamer pipeline  `[completed]`

**Recap chunków:**
- **Chunk A** (5f8ebd0+a7595ac): cargo deps (gstreamer 0.21 + gstreamer-app), migracja v21 `cameras`, docs/ADDON_HOST_FUNCTIONS.md §13, recipe `assets/test/sample_traffic.mp4`.
- **Chunk B** (27ef060+c9d10d5): `src/services/camera_ingest/` (fakefile + session + supervisor) — singleton tokio per kamera, GStreamer `filesrc ! decodebin ! videoconvert ! video/x-raw,format=RGB ! appsink`, replay loop, symlink guard po każdym komponencie path (Issue #6).
- **Chunk C** (c1f6eb7+976f19b): 10 host functions ABI (`camera_add_v1` ... `camera_credentials_rotate_v1`), SDK bindings (`tentaflow_addon_sdk::camera_*`), DB persistence przez `repository::*_camera`, audit log z RiskClass na każdej ścieżce, 24 unit testów w `tests/camera_host_functions.rs`.
- **Chunk D** (M1.W6 wrap): real WASM addon `addons/camera-test-addon/` + integration test `tests/camera_integration_e2e.rs` (4 testy, `#[ignore]` gated on WASM artifact + sample mp4), security suite `tests/camera_security.rs` (17 testów: SQL injection, length caps, symlink leaf/parent, traversal, special files, cross-addon isolation, supervisor vendor/fps guards).

**Coverage M1.W6:** 24 unit + 4 e2e + 17 security = **45 testów** camera API w F1a.

**Scope:**
- `tentaflow-core/src/services/camera_ingest/` (supervisor sesji tokio per kamera + registry)
- FakeFile connector (GStreamer: `filesrc ! decodebin ! videoconvert ! video/x-raw,format=RGB ! appsink`) z replay loop
- Host functions PEŁNE w `host_functions/camera.rs`:
  - `camera_add_v1` (F1a: tylko fake_file), `camera_list_v1`, `camera_get_v1`, `camera_update_v1`, `camera_remove_v1`
  - `camera_snapshot_v1` → SnapshotRef, `camera_health_v1` (fps/flags/last_seen)
  - `camera_discover_v1` (F1a empty Vec; F1b RTSP/ONVIF), `camera_test_connection_v1` (F1a fake_file only), `camera_credentials_rotate_v1` (F1a noop; F1b real)
  - Path traversal guard, vendor whitelist
- `assets/test/sample_traffic.mp4` (test data ~10MB, 5 min HD)

**Files:** `services/camera_ingest/` katalog, `host_functions/camera.rs` (~500L), Cargo (gstreamer 0.21 + gstreamer-app), 2 test files, addon-sdk bindings, `docs/ADDON_HOST_FUNCTIONS.md` sekcja 13 Camera API.

**Acceptance:** `camera_add({vendor:"fake_file", url:"/test/sample_traffic.mp4"})` → CameraId. List online z fps_actual ≈ target. `camera_snapshot` → SnapshotRef. Path traversal blocked. Vendor whitelist enforced.

### M1.W7 — Streaming + RawFrameRef + PickupToken + Service-to-Core API

**Scope:**
- Streaming bus `services/streaming/` z bounded mpsc per camera (capacity 100)
- LRU shared mem `services/frame_storage/` (1024 ramki/node default)
- RawFrameRef issuance: każda klatka → LRU + `frame_<uuid>`
- `stream_subscribe(target, filter)` → StreamId; `stream_next(id, timeout)` → StreamMessage; `stream_close(id)` → invalidate
- Backpressure: drop najstarsze + emit `Drop{count}` na resume
- PickupToken issuer `services/pickup_tokens.rs`:
  - HMAC SHA256, payload `{raw_ref, service_id, request_id, expiry, one_shot:true}` jako base64
  - DashMap TTL 30s, background cleanup co 60s
- Rozszerzenie `service_call_v1`: resolve alias przez router → wystaw PickupToken → wstrzyknij w QUIC payload → audit `alias_calls` entry (target_used, fallback_used, duration_ms, error_code)
- Service-to-Core API `/core/frame/pickup` w `api/frame_pickup.rs`:
  - POST z headers (X-Pickup-Token / X-Frame-Raw-Ref / X-Service-Id / X-Request-Id)
  - Weryfikacja HMAC + scope + one_shot consume
  - Zwraca bajty + metadata (width/height/codec/pts)
  - Audit `frame_pickup_log` entry

**Files:** `services/streaming/`, `services/frame_storage/`, `services/pickup_tokens.rs`, `api/frame_pickup.rs`, `host_functions/streaming.rs` (~300L), `host_functions/service.rs` extension, Cargo (dashmap, hmac, sha2), 2 test files (streaming_pickup + pickup_token_security), addon-sdk bindings, `docs/ADDON_HOST_FUNCTIONS.md` sekcja 14 Streaming + Service-to-Core.

**Acceptance:** Pełne e2e: FakeFile → stream_subscribe → stream_next → service_call → mock yolo pickup_frame → bbox response. Security: replay → 403, TTL 31s → 410, cross-service → 403, forge → 403. Backpressure detected. stream_next < 1ms p99, pickup < 20ms p99.

### M1.W8 — Recording basic + frame_url + audit chain hookup

**Scope:**
- Recording manager `services/recording/` (F1a basic — full ring-buffer/retention w F3):
  - Snapshot PNG do `~/.tentaflow/recordings/<camera_id>/snapshots/<uuid>.png`
  - Segment MP4 z GStreamer tee do `<camera_id>/segments/<uuid>.mp4`
  - W F1a brak automatic retention (manual `recording_purge`)
- HTTP handler `/recordings/<ref>?token=<sig>&exp=<ts>` z weryfikacją HMAC + expiry
- Host functions PEŁNE w `host_functions/recording.rs`:
  - `recording_save_segment_v1` → ClipRef + duration_ms + size + hash
  - `recording_save_snapshot_v1` → SnapshotRef
  - `recording_get_url_v1(ref, ttl_sec)` → signed URL + expires_at
  - `recording_get_stream_v1` (basic), `recording_purge_v1`, `recording_stats_v1`
- `frame_url_v1(raw_ref, ttl_sec)` — multi-use signed URL (inaczej niż PickupToken one-shot), TTL 60-600s
- Audit chain hookup basic: `recording_save_*` i `frame_url` → `audit_log_with_risk` z risk_class z `cameras.retention_class`

**Files:** `services/recording/`, `api/recording.rs`, `host_functions/recording.rs` (~400L), 2 test files, addon-sdk bindings, `docs/ADDON_HOST_FUNCTIONS.md` sekcja 15 Recording API + frame_url.

**Acceptance:** snapshot → SnapshotRef. get_url → signed URL. curl <url> → 200 PNG. Po expiry → 403. Token tampering → 403. `frame_url` multi-fetch w TTL OK. snapshot save < 50ms p99.

**M1 acceptance gate (koniec tyg. 8):**
- DoD-1, DoD-2, DoD-5, DoD-6, DoD-7, DoD-8, DoD-10, DoD-11, DoD-12 ✓
- Performance benchmarks (DoD-13): wszystkie metryki w targetach
- Coverage > 75% dla nowego kodu w M1
- 5 nowych sekcji w `docs/ADDON_HOST_FUNCTIONS.md`

---

## 7. Milestone M2 — UI M14/M15/M16 + integration tests (tyg. 9–11)

### M2.W9 — M16 frontend (Services → Aliasy global UI)

**Scope:** Strona `www/js/pages/services-aliases.js` w sekcji Services, tabs (Aliasy active), tabela 7-kolumnowa, filter chips, inline edit dialog z text input primary + strategy radio + fallback drag-to-reorder, manual create modal, tf-* components.

**Files:** `www/js/pages/services-aliases.{js,css}` (~600L), route registration.

**Acceptance:** Admin → /services/aliases pełna tabela, edit funkcjonuje, drag-to-reorder działa.

### M2.W10 — M14 readonly + M15 wizard kroki 1-3

**Scope:**
- M14 `www/js/pages/addons/tentavision/bindings.js`: tabs-bar (Bindings active), sekcja Aliasy (readonly z `alias_list_owned`), sekcja Storage (4 karty stats), link do M16
- M15 generic install wizard `www/js/pages/addons/install-wizard.js`: krok 1 Permissions (z manifest), krok 2 Storage (sql_backends choice), krok 3 Aliasy (status will-create/exists/conflict), kroki 4-6 placeholder F1a

**Files:** `bindings.js` (~400L), `install-wizard.js` (~500L), `api/addons/install.rs` extension.

**Acceptance:** M15 installuje TentaVision (kroki 1-3). M14 po install renderuje 6 aliasów + storage stats.

### M2.W11 — Integration tests + security tests + bug fixing

**Scope:** Integration suite (full install flow / alias CRUD / SQL CRUD / stream→service_call→pickup / recording save→URL). Security suite z planu §17.5 (~25 scenariuszy: pickup token replay/TTL/cross-service/forge, frame URL signing, path traversal × 3, FS isolation, SQL injection, quotas, DoS, manifest edge cases, migration partial/hash/existing DB, audit chain tamper).

**Files:** `tests/tentavision_integration/*.rs`, `tests/security/*.rs`.

**Acceptance:** `cargo test --workspace --release` zielony. 24 error codes triggered i sprawdzone.

**M2 acceptance gate:** DoD-3, DoD-4, DoD-9, DoD-10, DoD-15 basic ✓

---

## 8. Milestone M3 — Acceptance: UI e2e + perf + soak + release (tyg. 12–15)

### M3.W12 — UI e2e (Playwright)

**Scope:** Playwright setup, ~10 e2e tests z §17.6 (M14/M15/M16 pełne; M1/M3/M5/M6/M7/M11 placeholder check że renderują). Docker compose mock services. CI w GitHub Actions.

**Acceptance:** `npm run test:e2e` zielony. CI uruchamia per PR.

### M3.W13 — Performance benchmarks (Criterion)

**Scope:** 8 bench w `benches/` (service_call_overhead, stream_next, sql_insert/query, recording_snapshot, pickup roundtrip, pickup token issuance, migration apply). Vs targets z §17.8.

**Acceptance:** Wszystkie 8 w targetach. HTML report.

### M3.W14 — 24h soak + bug bash

**Scope:** TentaVision + 4 FakeFile cameras (różne profile/FPS) 24h. Monitoring RSS/CPU/FD/DB pool. Memory leaks (dhat). Bug fixing z M3.W12+W13.

**Acceptance:** Zero critical. RSS growth < 5% / 24h. No FD/DB pool leaks.

### M3.W15 — Release + handoff

**Scope:** `RELEASE-F1a.md` (release notes, breaking changes, teams-bot migration guide, known limitations). `notes/tentavision-f1a-acceptance-report.md` (DoD 17/17). `notes/tentavision-f1b-handoff.md` (pre-conditions F1b: RTSP/ONVIF design, lab cameras). Git tag `v0.1.0-f1a`. Stakeholder review.

**Acceptance:** Sign-off. Tag pushed. 17/17 DoD ✓.

**Demo M3 (acceptance ~30 min):**
1. Install TentaVision z marketplace via M15 wizard
2. 6 aliasów w M16, edit primary
3. FakeFile camera dodana via CLI (M3 UI placeholder)
4. Trigger CLI → service_call do mock yolo
5. M14 calls_24h=1, last_used_target
6. recording_save_snapshot → URL → obraz w browser
7. Audit log w M10 (placeholder UI) ma wszystkie operacje
- Performance dashboard live
- E2E suite live
- 24h soak grafy

---

## 9. Risk register

| # | Ryzyko | Likelihood | Impact | Mitigation |
|---|--------|-----------|--------|-----------|
| R1 | GStreamer dependency complexity (per-distro packaging) | High | Medium | Cargo features flag dla GStreamer; CI tests na Ubuntu 24.04 + Arch + Debian; Docker image z all deps prebuilt |
| R2 | FrameRef + PickupToken security model edge case (token reuse, race) | High | High | Comprehensive security test suite §17.5 zaplanowany na M2.W8; code review przed merge; external pentester po F1a (opcjonalnie) |
| R3 | SQLite per-addon scaling (jak addon ma 10M alarmów?) | Medium | Medium | F1a default 1M alarms acceptable; F4 vector + indexes; F8 PostgreSQL option dla dużych |
| R4 | Migration runner idempotency edge cases | Medium | High | Hash verification z M0.W3; comprehensive test scenarios w M3.W12 |
| R5 | TEAMS_BOT_ALIASES removal breaks existing deployments | High | High | Migration script + clear release notes + alpha testing z teams-bot maintainer |
| R6 | UI e2e flakiness na różnych przeglądarkach | Medium | Low | Playwright z Chromium primary; Firefox/Safari nice-to-have; retry strategy |
| R7 | Performance overhead service_call > 5ms p99 | Medium | High | Wczesny benchmark w M0.W2 stub; profile early, optimize hot path |
| R8 | Custom web components (z F1c) wymaga refactor M14 | Low | Low | M14 w F1a używa istniejących tf-* — F1c dopiero rozszerzenie |
| R9 | sdk_version mismatch między teams-bot a TentaVision z F1a | High | Medium | F1a release notes wymagają teams-bot update; coordination plan |
| R10 | F1a scope creep — dodawanie czegoś z F1b/F1c "tylko trochę" | High | High | Strict scope review per milestone; "deferred" lista (§15) chroniona |
| R11 | Audit chain implementation złożona (Merkle hash chain) | Medium | Medium | F1a basic z stub — pełne F2 |
| R12 | 16 tygodni real bo nieoczekiwane challenges | Medium | Medium | 4 tygodnie bufor w M4 (W13-W16 zawiera fixes + bench + acceptance) |

---

## 10. Test execution plan

### 11.1 Test pyramid F1a

```
              ┌─────────────────┐
              │  Manual / Demo  │  ~5 testów (acceptance demos per milestone)
              ├─────────────────┤
              │   E2E / UI      │  ~10 testów (Playwright M14/M15/M16 + placeholders)
              ├─────────────────┤
              │  Security tests │  ~25 testów (§17.5 — replay, traversal, quotas, DoS)
              ├─────────────────┤
              │  Integration    │  ~30 testów (§17.2 + custom)
              ├─────────────────┤
              │     Unit        │  ~150+ testów (każdy moduł)
              └─────────────────┘
```

### 11.2 Test infrastructure

- **Unit:** `cargo test` per crate, target coverage >80% dla nowego kodu
- **Integration:** `cargo test --test '*'` z testdata `assets/test/sample_traffic.mp4`
- **Security:** osobny test target `cargo test --test security`, uruchamiane w CI z `--release`
- **E2E:** Playwright + Docker compose mock services, headless w CI
- **Performance:** Criterion benchmarks, baseline po M2 jako reference, regression detection w CI
- **Soak:** 24h test w M4.W15 z monitoring (Prometheus + Grafana opcjonalnie)

### 11.3 CI pipeline

```yaml
# .github/workflows/f1a.yml
- name: Unit tests
  run: cargo test --workspace --lib
- name: Integration tests
  run: cargo test --workspace --test '*'
- name: Security tests
  run: cargo test --workspace --test security --release
- name: Build addon TentaVision
  run: cd tentaflow-core/addons-pro/tentavision && cargo build --target wasm32-wasip1 --release
- name: E2E tests
  run: docker compose -f tests/e2e/docker-compose.test.yml up -d && npm run test:e2e
- name: Benchmarks (smoke)
  run: cargo bench --no-fail-fast -- --quick
```

---

## 11. Tooling & infrastructure setup

### 12.1 Repo structure (po F1a)

```
TentaFlow/
├── tentaflow-core/
│   ├── src/
│   │   ├── addon/
│   │   │   ├── mod.rs                       # rozszerzony
│   │   │   ├── lifecycle.rs                 # manifest parser nowe sekcje
│   │   │   ├── manifest.rs                  # nowe structs
│   │   │   ├── migrations.rs                # NEW: migrations runner
│   │   │   ├── fs_sandbox.rs                # NEW
│   │   │   ├── storage_sql.rs               # NEW: per-addon SQLite mgmt
│   │   │   ├── sdk_version.rs               # NEW
│   │   │   ├── errors.rs                    # NEW: AbiError enum
│   │   │   └── host_functions/
│   │   │       ├── mod.rs                   # rejestracja
│   │   │       ├── service.rs               # rozszerzony (service_call_v1)
│   │   │       ├── sql.rs                   # NEW
│   │   │       ├── camera.rs                # NEW
│   │   │       ├── streaming.rs             # NEW
│   │   │       ├── aliases.rs               # NEW
│   │   │       └── recording.rs             # NEW
│   │   ├── services/
│   │   │   ├── camera_ingest/               # NEW: GStreamer-based
│   │   │   │   ├── mod.rs
│   │   │   │   ├── fake_file.rs             # F1a connector
│   │   │   │   └── registry.rs
│   │   │   ├── streaming/                   # NEW: streaming bus + RawFrameRef
│   │   │   ├── frame_storage/               # NEW: LRU shared mem
│   │   │   ├── pickup_tokens.rs             # NEW: HMAC token issuer
│   │   │   └── recording/                   # NEW: basic recording
│   │   ├── api/
│   │   │   ├── services/aliases.rs          # NEW: M16 backend
│   │   │   ├── frame_pickup.rs              # NEW: Service-to-Core API
│   │   │   ├── recording.rs                 # NEW: signed URL retrieval
│   │   │   └── addons/install.rs            # rozszerzony: multi-step wizard
│   │   └── db/
│   │       └── migrations.rs                # rozszerzona: model_alias_owners, alias_calls, ...
│   ├── www/
│   │   └── js/pages/
│   │       ├── services-aliases.js          # NEW: M16
│   │       ├── addons/
│   │       │   ├── tentavision/bindings.js  # NEW: M14
│   │       │   └── install-wizard.js        # NEW: M15 generic
│   │       └── ...
│   ├── benches/                             # NEW: criterion benchmarks
│   └── tests/
│       ├── tentavision_integration/         # NEW
│       ├── security/                        # NEW: §17.5 tests
│       └── e2e/                             # NEW: Playwright
├── tentaflow-core/addons/
│   └── test-app-addon/                      # istniejący, regression test
├── tentaflow-core/addons-pro/
│   ├── tentavision/                         # NEW: szkielet TentaVision F1a
│   │   ├── manifest.toml
│   │   ├── migrations/
│   │   │   └── 001_init.sql
│   │   ├── src/lib.rs
│   │   └── Cargo.toml
│   ├── teams-bot/                           # istniejący, do aktualizacji (manifest [[alias]])
│   ├── outlook/                             # istniejący
│   ├── sharepoint-rag/                      # istniejący
│   └── teams/                               # istniejący
├── tentaflow-cli/
│   └── src/commands/addon.rs                # NEW: validate command
└── notes/
    ├── tentavision-plan.md                  # v0.5.3
    ├── tentavision-f1a-implementation.md    # ten dokument
    ├── tentavision-sdk-research.md
    └── tentavision-plan-history-*.md
```

### 12.2 Dependencies dodane

```toml
# tentaflow-core/Cargo.toml dodatki
[dependencies]
gstreamer = "0.21"
gstreamer-app = "0.21"      # appsink dla frame extraction
r2d2 = "0.8"
r2d2_sqlite = "0.27"
dashmap = "6.1"              # PickupToken in-memory map
hmac = "0.12"                # PickupToken HMAC
sha2 = "0.10"

[dev-dependencies]
criterion = { version = "0.5", features = ["html_reports"] }
dhat = "0.3"                 # memory profiler
```

---

## 12. Communication cadence

### 13.1 Stand-ups

- **Daily** podczas M0-M2 (większość blockerów technicznych)
- **2x w tygodniu** podczas M3-M4

### 13.2 Milestone reviews

- Koniec każdego milestone: 1h review meeting
  - Demo dla stakeholderów
  - Acceptance gate checklist
  - Decyzja: go / no-go dla kolejnego milestone

### 13.3 Code review

- PR-based, każde nowe API/host function = osobny PR
- Required reviewers: 1 senior + 1 z security focus dla M2+ PR-ów (FrameRef/PickupToken)
- Auto-merge tylko po: CI green + 2 approvals + no critical comments

### 13.4 External reviews

- **Pentest** (opcjonalnie, po M2): zewnętrzny pentester sprawdza FrameRef + pickup tokens. Budget zarezerwowany
- **Architecture review**: po M0 + po M2, senior architect TentaFlow

---

## 13. F1a → F1b handoff plan

### 14.1 Pre-conditions dla F1b kickoff

- F1a tag released (`v0.1.0-f1a`)
- Acceptance demo zaakceptowane
- F1b backlog groomed (RTSP/ONVIF connector tasks)
- F1b lead assigned

### 14.2 F1b scope

- Real RTSP connector (GStreamer rtspsrc → decodebin → appsink)
- ONVIF Profile S/T discovery (WS-Discovery + SOAP)
- Camera vendor detection (Hikvision/Dahua/Axis quirks scanner)
- Production cameras tested z 1-2 fizycznymi w lab
- Reszta F1a infrastructure (alias mgmt, SQL, streaming, recording) bez zmian — F1b dodaje tylko nowe camera connectors

### 14.3 Co dziedziczymy z F1a do F1b

- Pełne SDK ABI (`service_call`, `stream_*`, `recording_*`, `sql_*`, `alias_*`)
- Per-addon FS + SQLite + migrations
- PickupToken + RawFrameRef infrastructure
- M14/M15/M16 v1 UI
- 6 aliasów TentaVision już w `model_aliases`
- Test infra (Playwright + Criterion + security tests)

### 14.4 Co F1b dodaje do TentaVision skel

- `camera_discover` zwraca prawdziwe RTSP/ONVIF kamery
- Production deployment guide (network config, VLAN, TLS)
- Acceptance test: 1 prawdziwa kamera RTSP zarejestrowana w TentaVision, service_call do mock yolo działa

---

## 14. Co celowo poza F1a (deferred do F1b/F1c/F2/F3)

Reminder co NIE robimy w F1a:

| Feature | Defer to | Powód |
|---------|----------|-------|
| Real RTSP / ONVIF | F1b | Wystarczy FakeFile dla MVP testing |
| Vendor-specific connectors (Hikvision/Dahua/Axis/Hanwha/...) | F8 | Long tail, każdy ma quirks |
| Custom UI components (Ed25519 + iframe sandbox) | F1c | Big infra effort, MVP UI używa tf-* |
| D1-D6 logic (modele inferencji) | F2-F7 | F1a tylko ABI/infrastructure |
| Policy / claims engine | F2 | F1a placeholder gate_check (zawsze passes) |
| Vector store full | F2 | F1a stub API zwraca empty |
| Flow invoke | F2 | F1a addon nie wywołuje Flow |
| Audit chain (Merkle hash + WORM) | F2 | F1a wpisuje do audit_log, F2 dodaje chain |
| Recording ring-buffer + retention | F3 | F1a save_snapshot/save_segment do plików, brak auto-purge |
| Evidence sign (HSM/TSA) | F3 | Long infra effort |
| D4 produkcja (face/reid) | F7 | After legal/audit infra |
| PostgreSQL backend | F8 | Optional, SQLite wystarczy 99% deploys |
| BTC anchoring | F10 | Nice-to-have, paid feature |
| Model rollback / ONNX upload UI | F8 | Different product surface |
| Multi-vendor UI component signing | F8 | Single signer (TentaFlow corp) wystarczy |

---

## Status dokumentu

**v0.1** — pierwsza iteracja po akceptacji planu v0.5.3
**Co dalej:**
1. ✅ Plan v0.5.3 zatwierdzony
2. ✅ Implementation plan v0.1 napisany (ten dokument)
3. ⏳ Decyzja: 1 senior 16 tygodni vs 2-os zespół 8 tygodni
4. ⏳ Assignment osób, kick-off M0.W1
5. ⏳ Tracking — Jira / Linear / GitHub Projects (do wyboru)
6. ⏳ External pentest budget zarezerwowany (~10k EUR)
