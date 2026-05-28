# 00 · Platform — Katalog ról

**Mockup:** [O2 Katalog ról](../../../../../.gstack/projects/Slyb00ts-TentaFlow/designs/crm-v1/o02-roles-catalog.html)

## Cel

Globalny, administrowalny katalog **ról funkcjonalnych** w organizacji. Rola to nie stanowisko — rola to *funkcja* osoby (np. „Handlowiec", „PM techniczny", „Architekt", „Sponsor wewnętrzny", „Decydent po stronie klienta"). Stanowiska (z [01 Org Structure](./01-platform-org-structure.md)) i `responsible_persons` na deal/projekcie referują role z tego katalogu.

**Rola opisuje *kto to jest* (atrybuty strukturalne), nie *co może robić* (akcje per addon).** Akcje są regułami w [02 Permissions](./02-platform-permissions.md) referującymi role. Ta separacja oznacza:
- Katalog ról jest mały, czysty, addon-agnostyczny
- Dodanie nowego addona nie wymaga przebudowy katalogu — addon definiuje własne reguły referując role które już są
- Te same role mogą być inaczej interpretowane przez różne addony

W IntrApp było 9 zahardkodowanych ról (`JobType` enum 1-9) + flagi typu `IsManager`, `CanEditProject`. Tutaj robimy CRUD — admin tworzy/edytuje/usuwa, bez deploymentu. Addon-specific flagi z IntrAppa migrują jako reguły w P2, nie jako pola roli.

## Domeny i encje

### `roles`

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `slug` | TEXT UNIQUE | Stabilna nazwa techniczna (np. `pm_technical`). Nie zmienia się po stworzeniu. |
| `name_translations` | JSONB | `{"pl": "PM techniczny", "en": "Technical PM", "de": "...", ...}` — wszystkie wspierane języki. Brak fallback per locale = błąd walidacji. |
| `description_translations` | JSONB | Analogicznie dla opisu. |
| `kind` | ENUM | `sales | technical | management | external | other` |
| `description` | TEXT | Co ta rola robi w organizacji |
| `icon` | TEXT | Nazwa ikony z biblioteki tf-* |
| `color_hint` | TEXT | Wskazówka dla UI (CSS var, np. `--accent-1`) |
| `is_active` | BOOL | Soft-delete |
| `created_at`, `updated_at` | TIMESTAMP | |

### Platformowe traity (kolumny `roles`)

Tylko strukturalne, addon-agnostyczne:

| Pole | Typ | Opis |
|---|---|---|
| `is_manager` | BOOL | Rola menedżerska. **Jedyny powód:** używane w O1 do layoutu drzewa (managerowie układają się wyżej, podwładni niżej). Nie wpływa na uprawnienia bezpośrednio — admin tworzy reguły w P2 referujące tę flagę jeśli chce. |
| `default_visibility_scope` | ENUM | Sugerowany scope dla addonów: `assigned | own | section | department | all`. Tylko podpowiedź — każdy addon może override w P2. |

**To wszystko.** Brak `can_edit_deal`, `can_close_deal`, `can_approve_budget`, `can_refacture_cost`, `can_log_time` — bo to akcje per addon, należące do P2 Permissions.

### Czego TUTAJ NIE MA (i dlaczego)

| Co zostało wyrzucone | Gdzie żyje teraz |
|---|---|
| `can_edit_deal`, `can_close_deal` | Reguła P2: `role=X, action=read+write, resource=deal, condition=owner_is_self` |
| `can_approve_budget` (z thresholdem) | Reguła P2 odwołująca się do `flag:is_manager` + warunek na deal.value_pln (lub osobna tabela `approval_thresholds_per_role` w CRM jeśli potrzeba) |
| `can_refacture_cost` | Reguła P2 w Billing addonie |
| `can_log_time` | Reguła P2 w przyszłym Timesheets addonie |
| `can_assign_others` | Reguła P2 w CRM (deal.responsible_persons.write) |
| `key_contact` | Tag/atrybut na `deal_client_contacts` lub `contact_roles_in_sales` |
| `see_all_in_*` | Reguły P2 z conditions: `section_of_self`, `dept_of_self`, transitive via org |

To pozwala: dodanie addona Timesheets nie zmienia katalogu ról — Timesheets w swoim manifeście tworzy reguły P2 referujące istniejące role.

## UI surfaces

### O2 Katalog ról (główny ekran)

**Sekcje:**
- Top: KPI strip (liczba ról per kind)
- Filtry: po kind
- Tabela: nazwa, kind, flagi (compact chips), default visibility, użycie (ile osób ma)
- Klik wiersza → otwiera **role editor** (panel poniżej tabeli)

**Akcje użytkownika:**

1. **"Nowa rola"** (przycisk top right)
   - Wyzwala: modal z formularzem (nazwa, kind, opis, flagi, scope)
   - Walidacja: slug auto-generowany ze name (sanitize), musi być unique
   - Po zapisie: INSERT do `roles`, INSERT do `role_flags` (z domyślnymi false), event `roles.created` (dla addonów żeby przeładowały selecty)

2. **Klik w wiersz → "Edytuj rolę"**
   - Wyzwala: panel edycji poniżej tabeli (jak w O2 mockup)
   - Inline: nazwa, kind, opis, ikona, toggle'e flag, threshold (warunkowo), scope
   - Po **"Zapisz"**: UPDATE `roles` + UPDATE `role_flags`, event `roles.updated`
   - **Side effect:** recalculation uprawnień we wszystkich addonach (host fn `permissions.recalculate`). Trwa ~5s, blokuje UI tylko dla tego rekordu, w audit log info „X osób miało zmianę".
   - Wyświetla licznik: „dotknie N osób aktualnie pełniących tę rolę"

3. **"Usuń rolę"**
   - Walidacja: nie da się usunąć jeśli `is_active=true` i istnieją wpisy `responsible_persons` referujące tę rolę. Wymaga wcześniejszej migracji wpisów na inną rolę (modal: „przenieś N osób na rolę X")
   - Po zapisie: `is_active=false` (soft delete), event `roles.deleted`

4. **Filtr po kind** (chipy w pasku filtrów)
   - URL state, restart sortowania

### Lista miejsc gdzie katalog jest używany

| Surface | Co robi |
|---|---|
| O1 Org Structure → edycja stanowiska | Select „rola dla tego stanowiska" |
| C3 Deal detail → „Dodaj osobę z rolą" | Modal z osobą + rolą (filtrowane do `kind = sales|technical|management`) |
| K2 Person detail → „Rola w sprzedaży" | Tag z roli z `kind=external` (dla osób klienta) |
| P2 Permissions matrix | Reguły uprawnień mogą referować rolę |
| Wszystkie addony | Read-only consume katalogu przez kontrakt `RolesProvider` |

## Provided contracts (manifest platformy)

To **nie jest** addon — to core platforma. Wystawia jednak swoje host fn jak każdy provider.

**Resources:**
- `role` (RolesProvider)

**Host functions:**
- `roles.list(filter: {kind?, is_active?}) → list<Role>`
- `roles.get(id) → Role | null`
- `roles.search(query) → list<Role>` (fuzzy po name_pl)
- `roles.create(input) → Role` (tylko admin grant)
- `roles.update(id, input) → Role` (tylko admin grant)
- `roles.deactivate(id) → ()` (soft-delete, tylko admin)

**Events:**
- `roles.created`
- `roles.updated` (zawiera diff i listę dotknięte osób)
- `roles.deactivated`

**Wszystkie addony mają domyślny grant `roles.read`** — nie ma sensu chronić katalogu, każdy addon i tak musi go widzieć żeby wyświetlić select. Write zarezerwowany dla admina.

## Permissions (kto co widzi)

- **Read** — wszyscy zalogowani (potrzebują do wyświetlania selectów ról w UI)
- **Write** — tylko `system.admin` rola w core TentaFlow

## Migracja z IntrApp

Seeduje katalog 14 ról + osobno seeduje reguły w P2.

### Role (sam katalog — tylko strukturalne)

**Sales (3):**
- `handlowiec_l1` — kind=sales, is_manager=false, default_scope=assigned
- `handlowiec_l2` — kind=sales, is_manager=false, default_scope=own
- `sales_lead` — kind=sales, is_manager=true, default_scope=section

**Technical (5):**
- `pm_technical` — kind=technical, default_scope=assigned
- `architect_senior` — kind=technical, default_scope=assigned
- `consultant_technical` — kind=technical, default_scope=assigned
- `developer` — kind=technical, default_scope=assigned
- `qa` — kind=technical, default_scope=assigned

**Management (3):**
- `section_director` — kind=management, is_manager=true, default_scope=section
- `sales_director` — kind=management, is_manager=true, default_scope=department
- `ceo` — kind=management, is_manager=true, default_scope=all

**External (3):**
- `decision_maker` — kind=external (key_contact: jest tagiem na contact_roles_in_sales, nie atrybutem roli)
- `influencer` — kind=external
- `power_user_sponsor` — kind=external

Pomijamy z IntrApp: `Porter` (5), `Keeper` (6), `Admin` (7 — to osobny RBAC), `Teacher` (8), `Support` (9).

### Towarzyszące reguły w P2 (seedowane osobno)

Reguły są szczegółowo opisane w [02 permissions](./02-platform-permissions.md), ale wstępna lista (subset):

- `handlowiec_l1/l2` → `read+write` `deal` gdy `owner_is_self`
- `pm_technical / architect / developer / qa` → `read+write` `deal` gdy `assigned_to_self`
- `flag:is_manager` → `read` `deal` gdy `subordinate`
- `sales_lead / section_director` → `read` `deal` gdy `section_of_self`
- `sales_director` → `read` `deal` gdy `dept_of_self` (transitive)
- `ceo` → `*` na `*` (see_everything z R005)
- `section_director` → `approve` `acceptance_card` gdy `deal.value_pln <= 500000`
- `sales_director` → `approve` `acceptance_card` gdy `deal.value_pln <= 2000000`
- `ceo` → `approve` `acceptance_card` zawsze
- `pm_technical / consultant_technical / developer / qa` → `write` `time_entry` gdy `assigned_to_project_of_entry` (gdy będzie Timesheets addon)

Każda reguła jest edytowalna przez admina po seed — to są tylko domyślne.

## Implementation order

1. Schema `roles` + `role_flags` + `role_default_visibility` (migracje DB).
2. Host fn CRUD (`roles.list/get/search/create/update/deactivate`).
3. Seed migracyjny 14 ról wstępnych.
4. UI O2 — tabela read + role editor.
5. UI O2 — create / update / soft-delete.
6. Walidacja przy `deactivate` — modal migracji osób.
7. Event publishing (`roles.*`).
8. Recalculation hook — gdy zmienia się flaga `is_manager` / `see_*` / `can_approve_budget`, wywołaj `permissions.recalculate` (z [02](./02-platform-permissions.md)).
9. Wpięcie selectu ról w O1 (Org), C3 (Deal), K2/K3 (Contacts).
10. Audit log na każdą zmianę roli.

## Otwarte decyzje

1. **Czy `default_visibility_scope` ma sens jeśli i tak addon override'uje?** Argument za: domyślna podpowiedź dla seedingu reguł. Argument przeciw: dodatkowy concept który nikt nie używa. Rekomendacja: **trzymamy, bo upraszcza migrację z IntrApp i daje adminowi quick-start przy tworzeniu nowej roli**.

2. **Czy admin może dodać własne flagi (oprócz `is_manager`)?** Pokusa: pozwolić addonom rejestrować własne booleany na roli (np. `can_present_at_demo` jako konwencja firmy). Rekomendacja: **NIE**. Wszystko co jest „may do X" idzie przez P2 rules. Trzymamy katalog ról minimalny.

3. **Wersjonowanie ról** — jeśli admin zmieni `name_pl` z „PM techniczny" na „Project Manager", co z historią (zatrudnieniami)? Rekomendacja: **slug stały, name_pl/name_en mogą się zmieniać; historia w `position_assignments` referuje `role_id` — etykieta ładuje się aktualną**. Audit log zapisuje zmiany nazwy.

4. **Multi-tenant** — w wersji enterprise jeden TentaFlow może obsługiwać kilka firm. Czy katalog ról jest per tenant czy globalny? Rekomendacja: **per tenant** (każda firma ma własne role).

5. **Localization** — **rozstrzygnięte: pełne wsparcie i18n od dnia 1.** `name_translations` i `description_translations` jako JSONB z kluczami języków. Lista wspieranych języków platformowa (`platform_locales` settings) — każda rola musi mieć wpis we wszystkich aktywnych językach (walidacja przy save). Domyślny język fallback ustawiany per-tenant. Język UI wybiera user w preferences.

6. **Reguły seedowe — czy zostawiać dla rolę po jej deactivate?** Jeśli admin usunie rolę `pm_technical`, czy reguły P2 referujące ją zostają (martwe) czy są też dezaktywowane? Rekomendacja: **przy `deactivate` role'a wszystkie reguły referujące ją są oznaczane `is_active=false` z notatką — admin może je przywrócić jeśli przywróci rolę**.
