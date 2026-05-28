# 00 · Katalog ról — plan implementacyjny

**Dokument źródłowy:** [00-platform-roles-catalog.md](./00-platform-roles-catalog.md)
**Mockup:** [O2 Katalog ról](../../../../../.gstack/projects/Slyb00ts-TentaFlow/designs/crm-v1/o02-roles-catalog.html)

**Cel kroku:** dostarczyć **produkcyjną** funkcjonalność katalogu ról jako fundament dla struktury organizacyjnej i permissions. Po tym kroku admin może w UI tworzyć/edytować/usuwać role; addony i org tool mogą referować role przez binary protocol.

**Zakres:** TYLKO katalog ról. NIE struktura, NIE permissions, NIE addony.

**Zasada nadrzędna:** kod produkcyjny od dnia 1. **Brak MVP, brak stubów, brak `// TODO`, brak `unimplemented!()`, brak fallbacków, brak skrótów.** Komunikacja **zawsze** przez binary protocol (`MessageBody`), zero REST.

## Realia stack'u (poprawione)

- **DB to SQLite** (rusqlite, nie PostgreSQL!). Migracje w `src/db/migrations.rs` jako `Vec<(version, name, MigrationStep)>`. Następny wolny numer: **40** (po `(39, "scheduled_jobs", ...)`).
- **Brak PostgreSQL ENUM, JSONB** — zastępujemy `CHECK (col IN (...))` i `TEXT` zawierający JSON (SQLite ma `json_*()` funkcje).
- **Istnieje `services/org/`** z multi-tenant orgs + auth roles (admin/member/viewer z permissions strings). Nasz katalog ról to **inna domena** (role biznesowe / typy stanowisk). Nazwa modułu: **`services/role_catalog/`** żeby uniknąć kolizji.
- **Auth/admin check:** istnieje wzorzec `is_admin(ctx: &HandlerContext) -> bool` w `src/api/dashboard/`. Używamy go bezpośrednio.
- **Audit log:** istnieje tabela `audit_log` używana w `services/legal/`, `services/mesh_keys/`, `services/service_call.rs`. Wzorzec INSERT z polami: user_id, addon_id, instance_id, action, resource_type, resource_id, result, action_hash, risk_class, request_id, timestamp, prev_hash, hash, org_id.
- **Binary protocol:** `tentaflow-protocol/src/message_body.rs` — `pub enum MessageBody` z handrolled wariantami per operation (`RoleCatalogListRequest`, `RoleCatalogListResponse`, ...). Używa `CBOR::Archive, Deserialize, Serialize`. Typy w `types.rs`.

---

## Założenia stałe

- **i18n od dnia 1** — `name_translations` i `description_translations` jako JSONB, walidacja kompletności wszystkich aktywnych języków
- **Multi-tenant od dnia 1** — wszystko per-tenant, FK na `tenant_id` we wszystkich tabelach
- **Brak addon-specific flag** — tylko `is_manager` + `default_visibility_scope`
- **Soft delete** — `is_active=false`, brak DELETE
- **Audit log** — każda zmiana w `audit_log` (już istnieje w TentaFlow)

---

## Faza 0 — Decyzje techniczne (przed kodem)

Te muszą być rozstrzygnięte przed pierwszym commitem:

| # | Decyzja | Rekomendacja |
|---|---|---|
| D1 | Migracje DB — czy używamy istniejącej infrastruktury TentaFlow (`migrations/` w `tentaflow-core`)? | TAK — kolejna migracja w sekwencji (np. `25_roles_catalog.sql`) |
| D2 | `default_visibility_scope` jako enum w SQL czy w aplikacji? | **Enum w SQL** (PostgreSQL ENUM type) — typowanie, indeksacja |
| D3 | Walidacja kompletności translacji — w DB constraint czy aplikacji? | **W aplikacji** (host fn `create/update` sprawdza). DB constraint za skomplikowany dla JSONB. |
| D4 | Lista aktywnych języków platformowych — gdzie? | Tabela `platform_locales` (id, code, name, is_default) per-tenant. Seed: `pl` (default), `en`. |
| D5 | Host fn naming — `roles.list` czy `tenta.roles.list`? | Bez prefiksu — namespacing przez moduł WASM. Zgodne z istniejącymi host fn (`http.request`, `sql.query`). |
| D6 | API stylu — REST czy binary protocol (`MessageBody`)? | **Binary protocol** — zgodnie z CLAUDE.md i ustaloną Tier 1 architekturą TentaFlow. |
| D7 | Co z istniejącymi tabelami `Jobs` z IntrApp w bazie production? | Migracja danych w **osobnym kroku** (`00-migrate-intrapp.md`) — nie blokuje rozwoju 00. |

---

## Faza 1 — Schema bazy (1-2 dni)

### Migracja `migrations/25_roles_catalog.sql`

```sql
-- 1. Enum dla visibility scope
CREATE TYPE role_visibility_scope AS ENUM (
  'assigned', 'own', 'section', 'department', 'all'
);

-- 2. Enum dla kind
CREATE TYPE role_kind AS ENUM (
  'sales', 'technical', 'management', 'external', 'other'
);

-- 3. Tabela platformowych języków (per-tenant)
CREATE TABLE platform_locales (
  id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id   UUID NOT NULL,
  code        TEXT NOT NULL,            -- ISO 639-1: pl, en, de, fr...
  display_name TEXT NOT NULL,           -- "Polski", "English"
  is_default  BOOLEAN NOT NULL DEFAULT false,
  is_active   BOOLEAN NOT NULL DEFAULT true,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, code)
);
-- Wymóg: jeden default per tenant
CREATE UNIQUE INDEX platform_locales_one_default_per_tenant
  ON platform_locales (tenant_id) WHERE is_default = true;

-- 4. Katalog ról
CREATE TABLE roles (
  id                        UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id                UUID NOT NULL,
  slug                      TEXT NOT NULL,
  kind                      role_kind NOT NULL,
  name_translations        JSONB NOT NULL,   -- {pl: "...", en: "...", ...}
  description_translations JSONB NOT NULL DEFAULT '{}'::jsonb,
  icon                      TEXT,             -- ikona z tf-* biblioteki
  color_hint               TEXT,             -- "--accent-1" lub hex
  is_manager               BOOLEAN NOT NULL DEFAULT false,
  default_visibility_scope role_visibility_scope NOT NULL DEFAULT 'assigned',
  is_active                BOOLEAN NOT NULL DEFAULT true,
  created_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
  created_by               UUID,             -- user_id
  UNIQUE (tenant_id, slug)
);

CREATE INDEX roles_tenant_active   ON roles (tenant_id, is_active);
CREATE INDEX roles_tenant_kind     ON roles (tenant_id, kind);

-- 5. Trigger na updated_at
CREATE OR REPLACE FUNCTION set_updated_at() RETURNS TRIGGER AS $$
BEGIN NEW.updated_at = now(); RETURN NEW; END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER roles_set_updated_at
  BEFORE UPDATE ON roles
  FOR EACH ROW EXECUTE FUNCTION set_updated_at();
```

### Test schema

- Insert minimalnego roli, GET, UPDATE, soft-delete
- Insert z brakującym językiem default — powinno przejść (walidacja w app)
- 2 role z tym samym slug w tym samym tenant — błąd UNIQUE

---

## Faza 2 — Host functions (3-4 dni)

Dodajemy do `tentaflow-core/src/`. Plik: `src/services/roles_catalog.rs`.

### 2.1 Funkcje read (start)

```rust
// Pseudocode — pełen sygnatury

/// Lista ról z filtrami
pub async fn roles_list(
    tenant_id: Uuid,
    filter: RolesListFilter,
) -> Result<Vec<Role>, Error>;

pub struct RolesListFilter {
    pub kind: Option<RoleKind>,
    pub is_active: Option<bool>,
    pub search: Option<String>, // fuzzy po name_translations
}

pub async fn roles_get(tenant_id: Uuid, id: Uuid) -> Result<Option<Role>, Error>;

pub async fn roles_get_by_slug(tenant_id: Uuid, slug: &str) -> Result<Option<Role>, Error>;
```

**Walidacja:** każdy call sprawdza `tenant_id` zgodności z sesją usera.

### 2.2 Funkcje write

```rust
pub async fn roles_create(
    tenant_id: Uuid,
    actor: Uuid,
    input: RoleCreateInput,
) -> Result<Role, Error>;

pub struct RoleCreateInput {
    pub slug: String,                   // sanitize: lowercase, snake_case
    pub kind: RoleKind,
    pub name_translations: Map<String, String>,
    pub description_translations: Map<String, String>,
    pub icon: Option<String>,
    pub color_hint: Option<String>,
    pub is_manager: bool,
    pub default_visibility_scope: VisibilityScope,
}

pub async fn roles_update(
    tenant_id: Uuid,
    actor: Uuid,
    id: Uuid,
    patch: RoleUpdateInput,
) -> Result<Role, Error>;

pub async fn roles_deactivate(
    tenant_id: Uuid,
    actor: Uuid,
    id: Uuid,
) -> Result<(), Error>;
```

**Walidacja w create/update:**
1. `slug` non-empty, regex `^[a-z][a-z0-9_]*$`, max 50 znaków
2. `name_translations` musi mieć klucze dla **wszystkich aktywnych locale** danego tenantu (zapytanie do `platform_locales`)
3. Każdy translation non-empty
4. `description_translations` — opcjonalnie, ale jeśli podane, musi być kompletne
5. `icon` jeśli podana, musi być w whitelist (lista ikon `tf-*`)
6. `color_hint` jeśli podany, regex hex lub `--<css-var>`
7. Audit log entry przy każdym create/update/deactivate

**Walidacja przy deactivate:**
1. Brak — soft delete zawsze przechodzi
2. Side-effect: emit event `roles.deactivated`
3. Konsekwencje w P2 (permissions) — addony zareagują same przez subscription

### 2.3 Event publishing

```rust
// Eventy idą przez istniejący event bus TentaFlow
pub enum RolesEvent {
    Created(Role),
    Updated { before: Role, after: Role, diff: JsonValue },
    Deactivated { role_id: Uuid, deactivated_by: Uuid },
}
```

Każda mutacja emituje odpowiedni event z payloadem.

### 2.4 Binary protocol — MessageBody

W `tentaflow-protocol`:

```rust
#[derive(Serialize, Deserialize)]
pub enum RolesPayload {
    List(RolesListRequest),
    Get { id: Uuid },
    GetBySlug { slug: String },
    Create(RoleCreateInput),
    Update { id: Uuid, patch: RoleUpdateInput },
    Deactivate { id: Uuid },
    // responses
    ListResponse(Vec<Role>),
    Role(Option<Role>),
    Error(String),
}
```

Dispatch w `dashboard.rs` / `unified_server.rs` — gałąź `MessageBody::Roles(payload)`.

### 2.5 Testy host fn

`tests/services/roles_catalog_test.rs` — `cargo test`:

- create_basic / create_missing_translation / create_invalid_slug / create_duplicate_slug
- update_partial / update_with_invalid_translation
- deactivate / deactivate_already_inactive
- list_filtered_by_kind / list_search_by_name
- get_by_slug / get_nonexistent
- multi-tenant isolation (tenant A nie widzi ról tenant B)

---

## Faza 3 — Seed migracji (1 dzień)

Plik `migrations/26_seed_roles.sql` — uruchamiany tylko na świeżej bazie lub gdy `roles` jest pusta:

```sql
-- Seed platform_locales (per tenant — przykład dla default tenant)
INSERT INTO platform_locales (tenant_id, code, display_name, is_default) VALUES
  ('00000000-0000-0000-0000-000000000001', 'pl', 'Polski', true),
  ('00000000-0000-0000-0000-000000000001', 'en', 'English', false);

-- Seed 14 ról z LIFECYCLE.md
INSERT INTO roles (tenant_id, slug, kind, name_translations, description_translations, icon, is_manager, default_visibility_scope) VALUES
-- Sales
('00000000-0000-0000-0000-000000000001', 'handlowiec_l1', 'sales',
 '{"pl":"Handlowiec L1","en":"Sales Rep L1"}'::jsonb,
 '{"pl":"Junior · podstawowa rola sprzedażowa","en":"Junior · entry-level sales"}'::jsonb,
 'i-briefcase', false, 'assigned'),
('00000000-0000-0000-0000-000000000001', 'handlowiec_l2', 'sales',
 '{"pl":"Handlowiec L2","en":"Sales Rep L2"}'::jsonb,
 '{"pl":"Senior · samodzielny","en":"Senior · independent"}'::jsonb,
 'i-briefcase', false, 'own'),
('00000000-0000-0000-0000-000000000001', 'sales_lead', 'sales',
 '{"pl":"Sales Lead","en":"Sales Lead"}'::jsonb,
 '{"pl":"Lider zespołu sprzedażowego","en":"Sales team leader"}'::jsonb,
 'i-briefcase', true, 'section'),
-- Technical (5)
('00000000-0000-0000-0000-000000000001', 'pm_technical', 'technical', ...),
-- ... pełna lista 14 ról jak w 00-platform-roles-catalog.md sekcja Migracja
;
```

**Idempotentność:** seed sprawdza `WHERE NOT EXISTS (SELECT 1 FROM roles WHERE tenant_id=...)` przed insertem.

---

## Faza 4 — UI (3-4 dni)

W `tentaflow-core/www/js/modules/`. Nowy plik: `roles_catalog.js`.

### 4.1 Routing

W globalnym menu TentaFlow (sekcja Zarządzanie) dodać item „Struktura organizacyjna → Katalog ról" → `?view=roles-catalog`. (Sam org tool będzie w kroku 01.)

### 4.2 Komponenty

Wszystko z istniejącej biblioteki `tf-*`:
- `tf-table` — lista ról (kolumny zgodne z mockupem O2)
- `tf-input`, `tf-select`, `tf-toggle` — formularz
- `tf-window` — modal Edycja roli / Nowa rola
- `tf-chip` — kind, scope
- `tf-button`

### 4.3 Stan widoku

```js
class RolesCatalogView {
  state = {
    roles: [],
    activeFilter: 'all' | 'sales' | 'technical' | 'management' | 'external',
    editingRole: null,    // Role | null
    searchQuery: '',
    isLoading: false,
    locales: [],          // z platform_locales
  };

  async mount() {
    this.state.locales = await api.locales.list();
    await this.refresh();
  }

  async refresh() {
    this.state.roles = await api.roles.list({
      kind: this.state.activeFilter !== 'all' ? this.state.activeFilter : undefined,
      is_active: true,
      search: this.state.searchQuery || undefined,
    });
    this.render();
  }
}
```

### 4.4 Akcje

- Klik wiersza → `openEditor(role)`
- „+ Nowa rola" → `openEditor(null)`
- W edytorze: walidacja inline (każdy język w `name_translations` non-empty); zapis przez `api.roles.create` lub `api.roles.update`; po sukcesie modal się zamyka + `refresh()`
- „Usuń rolę" → confirm dialog + `api.roles.deactivate`; po sukcesie refresh

### 4.5 Form editor — i18n inputs

Dla każdego aktywnego locale tabki (Polski, English, ...) z polami nazwa + opis. Toolbar nad polami: „Pokaż wszystkie języki" / „Tylko aktywny". Walidacja: zielony tick przy języku z kompletem, czerwony marker gdy puste.

### 4.6 Test ręczny

- Stworzyć rolę „Test Role" w PL + EN → zapisz → pojawia się w tabeli
- Edytuj → zmień nazwę PL → save → tabela aktualna
- Usuń → confirm → znika z listy aktywnych
- Filtry kind → tylko odpowiednie role
- Search „handl" → tylko handlowcy

---

## Faza 5 — Audit log integration (0.5 dnia)

Wpis do istniejącej tabeli `audit_log` (TentaFlow ma już infrastrukturę) przy każdej mutacji:

```json
{
  "actor_user_id": "...",
  "tenant_id": "...",
  "addon_instance": "platform.roles_catalog",
  "action": "role_created" | "role_updated" | "role_deactivated",
  "resource_type": "role",
  "resource_id": "...",
  "before": {...},       // tylko dla update/deactivate
  "after": {...},        // tylko dla create/update
  "details": {...},
  "result": "success",
  "ip_address": "...",
  "user_agent": "..."
}
```

---

## Faza 6 — Event subscription smoke-test (0.5 dnia)

Dummy consumer (test-only) — addon słuchający `roles.*` eventów + loguje do stdout. Sprawdzić:
- Create role → event przychodzi z pełnym payloadem
- Update → event z diffem (before/after)
- Deactivate → event z `role_id` + `deactivated_by`

To zapewnia że event bus działa dla naszych pierwszych eventów platformowych. Późniejsze addony będą subskrybować realnie.

---

## Faza 7 — Permissions na sam katalog (0.5 dnia)

Tymczasowe rozwiązanie zanim mamy [02 Permissions](./02-platform-permissions.md):

```rust
fn check_admin(actor: Uuid, tenant_id: Uuid) -> Result<(), Error> {
    // sprawdza czy user ma rolę "admin" w TentaFlow auth
    // (istniejąca infrastruktura TentaFlow)
}
```

Wszystkie write functions (`create`, `update`, `deactivate`) wymagają `check_admin`. Read dostępne dla każdego zalogowanego (`tenant_id` matchuje).

Po implementacji P2 (krok 2) — zastąpimy na proper rules.

---

## Definition of Done dla kroku 00

- [ ] Migracja DB `25_roles_catalog.sql` deployowana, schema utworzona
- [ ] Seed locale (pl, en) + 14 ról IntrAppowych załadowane
- [ ] Host fn `roles.list/get/get_by_slug/create/update/deactivate` zaimplementowane
- [ ] Walidacja kompletności `name_translations` dla wszystkich aktywnych locale
- [ ] Binary protocol `RolesPayload` + dispatcher w unified_server
- [ ] UI O2 wdrożony: tabela + filtry + search + modal editor (zgodny z mockupem)
- [ ] Audit log entry przy każdej mutacji
- [ ] Event publishing — 3 eventy
- [ ] Testy unit: 12+ scenariuszy host fn
- [ ] Test ręczny UI: create / update / delete / filter / search
- [ ] Admin auth check tymczasowy (zastąpiony przy 02)
- [ ] Brak hardcoded ról / typów w kodzie — wszystko z katalogu

---

## Otwarte pytania do rozstrzygnięcia w trakcie

1. **Pierwszy język w edytorze** — który jest domyślnie otwarty? Locale defaultowy tenantu, czy ostatnio używany przez usera?
2. **Sortowanie w tabeli** — default po `kind` potem `name` (current locale)? Czy po `created_at DESC`?
3. **Limit liczby ról** — sanity check (np. 200 per tenant) czy bez limitu?
4. **Dezaktywowane role w UI** — pokazujemy w osobnej zakładce „Zarchiwizowane" czy ukrywamy zupełnie? Rekomendacja: ukrywamy, ale admin może przełączyć toggle „Pokaż nieaktywne".

---

## Następny krok po 00

Jak tylko katalog jest stabilny i wdrożony (Definition of Done check), zaczynamy **01 Struktura organizacyjna** ([01-impl.md](./01-impl.md) — do napisania) — to gruba robota głównie przez `tf-org-tree` komponent (canvas SVG z zoom/pan/drag-drop, ~2-3 tygodnie samego komponentu).
