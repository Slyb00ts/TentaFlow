# 02 · Platform — Permissions engine

**Mockup:** [P2 Permissions matrix](../../../../../.gstack/projects/Slyb00ts-TentaFlow/designs/crm-v1/p02-permissions.html)

## Cel

Silnik liczenia **effective access** dla każdego user × resource. Łączy trzy osie:

1. **User** — bezpośrednie nadania (rzadkie, dla wyjątków)
2. **Group** — grupy RBAC (jak „Wszyscy admini", „QA team")
3. **Position w org structure** — wynik z [01](./01-platform-org-structure.md) (najczęstsze)

Plus **per-instance grants między addonami** (jak `crm/main → contacts.read_basic`) — to ortogonalna oś dla komunikacji programowej (nie userów).

## Domeny i encje

### `groups`

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `name` | TEXT | „Sales team", „All staff" |
| `kind` | ENUM | `static | dynamic` |
| `rule` | JSONB | Dla dynamic — predykat (np. `{role: "handlowiec_l2"}` — auto-członkostwo wszystkich z tą rolą) |

### `group_members`

| Pole | Typ | Opis |
|---|---|---|
| `group_id` | UUID FK | |
| `person_id` | UUID FK | |
| `added_at`, `added_by` | | |

Dla `kind=dynamic` ta tabela jest **materialized view** odświeżany triggerem.

### `permission_rules`

Reguła = warunek + dozwolona akcja na zasobie.

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `name` | TEXT | „Handlowcy widzą swoje deale" |
| `resource_type` | TEXT | `deal`, `contact`, `invoice`, `*` |
| `action` | ENUM | `read | write | delete | approve | export | *` |
| `subject` | JSONB | KTO ma to prawo. Schemas niżej. |
| `condition` | JSONB | NA CO ma to prawo (predicate na zasobie). |
| `effect` | ENUM | `allow | deny` |
| `priority` | INT | Reguły z wyższym priority wygrywają. `deny` z tym samym priority bije `allow`. |
| `is_active` | BOOL | |
| `created_by`, `created_at` | | |

**Subject schemas (kto):**
- `{kind: "user", user_id: UUID}` — konkretny user
- `{kind: "group", group_id: UUID}` — wszyscy w grupie
- `{kind: "role", role_id: UUID}` — wszyscy z tą rolą (na dowolnym stanowisku)
- `{kind: "position", position_id: UUID}` — kto siedzi na tym stanowisku
- `{kind: "everyone"}` — wszyscy zalogowani
- `{kind: "anyone_with_flag", flag: "is_manager"}` — wszyscy mający flagę z roli

**Condition schemas (na co):**
- `{}` — zawsze (na wszystkich zasobach tego typu)
- `{owner_is_self: true}` — gdy zasób ma `owner_id = current_user_id`
- `{assigned_to_self: true}` — gdy current user w `responsible_persons` zasobu
- `{section_of_self: true}` — gdy section/department zasobu = section/dept current user position
- `{subordinate: true}` — gdy owner/assignee zasobu jest subordinate current user (via `org.is_subordinate_of`)
- `{matches: {section_id: "$user.section"}}` — explicit field match
- AND/OR/NOT — kompozyty: `{and: [...], or: [...]}`

### `permission_rule_overrides` (per-instance grants między addonami)

To inna oś — dla programowej komunikacji addonów. NIE używa subject/condition.

| Pole | Typ | Opis |
|---|---|---|
| `grantor_addon_instance` | TEXT | Np. `contacts/main` (ten kto ma zasób) |
| `grantee_addon_instance` | TEXT | Np. `crm/main` (ten kto chce dostęp) |
| `capability` | TEXT | Np. `read_basic`, `write_relations` |
| `granted_at`, `granted_by` | | |
| `is_active` | BOOL | |

### `effective_access` (materialized cache)

| Pole | Typ | Opis |
|---|---|---|
| `user_id` | UUID | |
| `resource_type` | TEXT | |
| `resource_id` | UUID | (lub `*` dla wildcardu) |
| `actions` | TEXT[] | `['read', 'write']` |
| `granted_via` | JSONB | Lista reguł które to dały (do debuggingu) |
| `computed_at` | TIMESTAMP | |

Cache jest **incremental** — odświeżany triggerem przy zmianach `permission_rules` / `position_assignments` / `roles` / `group_members` lub przy zmianach samego zasobu (np. dodanie nowego deala). Dla małych firm (~50 userów × 1k deali) cała tabela może być przeliczona w &lt;5s.

## UI surfaces

### P2 Permissions matrix (główny ekran admin)

**Layout:** macierz N × M (capabilities × addony) + sekcja `permission_rules` + sekcja grup.

**Sekcje:**

1. **Inter-addon grants** (macierz programowa) — istniejący widok z mockupu P2
   - Wiersze: capabilities (`contacts.read_basic`, `crm.deal.write` itd.)
   - Kolumny: addon instances
   - Komórka: granted / pending / brak
   - Klik komórki → modal grant/revoke

2. **User-facing permission rules** (nowa sekcja)
   - Tabela reguł: nazwa, subject (kto), action, resource_type, condition (kompakt), priority, status
   - Filtr po resource_type, subject kind
   - "+ Nowa reguła" → wizard (subject → action → resource → condition)

3. **Groups** (osobny tab, możliwy ten sam ekran)
   - Tabela: grupa, kind, członków, użyta w X regułach
   - CRUD na grupach

### P4 Effective Access Inspector (debugowanie — nowy ekran nie mockowany jeszcze)

Wpisz user + resource → pokaż:
- Czy ma dostęp (read/write)
- Lista reguł które zadziałały (allow + deny)
- Ścieżka przez org/group/role (czemu)

**Krytyczne dla supportu** — bez tego nie da się odpowiedzieć „dlaczego Anna nie widzi deala X".

### Modal „Nowa reguła"

Wizard 4 kroki:

1. **Subject** — radio: user/group/role/position/everyone/flag → odpowiedni select
2. **Action** — multi-select: read/write/delete/approve/export
3. **Resource** — type + ewentualnie konkretne ID (rzadko)
4. **Condition** — visual builder predicates (drag-drop AND/OR + atomic checks like `owner_is_self`, `subordinate`, `section_of_self`, custom field match)
5. **Priority + effect** — slider (1-1000) i radio allow/deny

Po zapisie: INSERT `permission_rules` + invalidate `effective_access` dla dotkniętych zasobów/userów.

### Default reguły (preinstalowane)

Po seed migracji są wpisane:

- **R001 (priority 100, allow)** — `everyone` może `read` `contact` gdzie `condition: {}` (wszyscy widzą wszystkie kontakty)
- **R002 (priority 100, allow)** — `role=handlowiec_l1/l2` może `read+write` `deal` gdzie `condition: {owner_is_self: true}`
- **R003 (priority 200, allow)** — `flag=is_manager` może `read` `deal` gdzie `condition: {subordinate: true}`
- **R004 (priority 200, allow)** — `flag=see_all_in_section` może `read` `deal` gdzie `condition: {section_of_self: true}`
- **R005 (priority 300, allow)** — `flag=see_everything` (CEO) może `*` na `*` z `condition: {}`
- **R006 (priority 500, allow)** — `flag=can_approve_budget` może `approve` `acceptance_card` gdzie `condition: {amount_within_threshold: true}`
- **R007 (priority 200, allow)** — `role.kind=technical AND assigned_to_self=true` może `read+write` na `deal`

Admin może te reguły edytować / wyłączyć / dodać własne.

## Provided contracts (host fn)

**Read:**
- `permissions.can(user_id, action, resource_type, resource_id) → bool` — pojedynczy check
- `permissions.list_for_user(user_id, resource_type, action) → list<resource_id>` — co user widzi (dla filtra w listach addonowych)
- `permissions.explain(user_id, resource_type, resource_id) → ExplainResult` — debug: które reguły zadecydowały
- `permissions.bulk_check(user_id, [(action, resource_type, resource_id), ...]) → list<bool>` — batch dla list

**Write (admin):**
- `permissions.create_rule(input) → Rule`
- `permissions.update_rule(id, input)`
- `permissions.delete_rule(id)`
- `permissions.recalculate(scope: {users?, resources?, all?}) → JobId` — wymusza pełny recompute (async)

**Groups (admin):**
- `groups.create/update/delete`
- `groups.add_member` / `groups.remove_member`
- Dla dynamic groups: `groups.evaluate_rule(rule)` — preview kto by się załapał

**Inter-addon grants (admin):**
- `grants.create(grantor, grantee, capability)`
- `grants.revoke(grant_id)`
- `grants.list(grantee_addon)` — co dany addon ma dozwolone

**Events:**
- `permissions.rule_created/updated/deleted`
- `permissions.recalculated` (z metryką: ile rekordów, jak długo)

## Algorytm `permissions.can`

```
1. Wczytaj user (z position + role + groups)
2. Wczytaj zasób (z fields: owner_id, section_id, responsible_persons, etc.)
3. Pobierz wszystkie reguły matchingowe:
   - subject matches user (user_id / group_id / role_id / position_id / everyone / flag)
   - resource_type matches
   - action ∈ rule.actions (lub action='*')
   - condition jest spełniona dla tego zasobu
4. Posortuj po priority DESC. Pierwszy match wygrywa.
5. Jeśli effect=deny → false. effect=allow → true.
6. Brak match → false (default deny).
```

Dla wydajności w listingach: użyj `effective_access` cache + invalidate przy zmianach.

## Permissions for permissions (rekursja)

- **Read reguł** — admin + wszyscy z rolą `system.security_auditor`
- **Write reguł** — tylko admin (`system.admin`)
- **Read effective_access dla siebie** — każdy user dla siebie (P4 inspector dostępny dla każdego — przydatne diagnostycznie)
- **Read effective_access dla innych** — admin

## Sztywne reguły platformowe (zaszyte w kodzie, nie konfigurowalne)

Wszystkie narzędzia administracyjne mają **stałą regułę: tylko `system.admin` może pisać**. Te reguły są wymuszone na poziomie handlerów (`require_admin(ctx)`), niezależnie od `permission_rules` — nawet jeśli admin doda regułę dającą innej roli prawa, write na te zasoby będzie odrzucony przez warstwę handler.

| Zasób | Read | Write |
|---|---|---|
| `role_catalog` | wszyscy zalogowani | `system.admin` only |
| `position` / `position_reports_to` / `position_assignments` (Org Structure) | wszyscy zalogowani | `system.admin` only |
| `permission_rules` | admin + `system.security_auditor` | `system.admin` only |
| `groups` / `group_members` | admin | `system.admin` only |
| `ai_tools` (registry) | admin | `system.admin` only |
| `permission_rule_overrides` (inter-addon grants) | admin | `system.admin` only |
| `platform_locales` | wszyscy zalogowani | `system.admin` only |

**Dlaczego nie konfigurowalne:**
- Te reguły dotyczą bezpieczeństwa platformy. Pozwolenie nie-adminowi na edycję uprawnień (P2) = privilege escalation.
- Pozwolenie nie-adminowi na edycję org structure = manipulacja widoczności danych.
- Pozwolenie nie-adminowi na edycję `ai_tools` = możliwość zarejestrowania tool'a robiącego cokolwiek.

User i Power User (nawet z `flag:see_everything`) nie mają write na te zasoby. Jeśli organizacja chce delegować — daje komuś admin role w TentaFlow auth (osobna decyzja personalna, nie reguła permissions).

## Migracja z IntrApp

IntrApp miał luźny system z `UserGroupRoles` (tabela dynamicznie generowana). Każda kolumna = uprawnienie boolean. Plus filtry inline w SQL (`session.User.Roles.Documents.Projects.All.Read`).

Mapowanie:
- `session.User.Roles.Documents.Projects.All.Read = true` → reguła R005-like dla danego usera/grupy
- `session.User.Roles.Documents.Projects.Read = true` → R002 + R007 odpowiednio (visibility from roles)
- `session.User.JobType == 3` (PM) → automatycznie poprzez R007 (rola pm_technical + assigned_to_self)
- `Config.UseProject > 0` (tryb CRM) — w nowym tylko jeden tryb, nie ma flagi

Skrypt migracji: dla każdego usera IntrApp wyciągnij jego flagi z `UserGroupRoles`, zmapuj na reguły TentaFlow (głównie wpisy do `group_members` w odpowiednich predefiniowanych grupach).

## Implementation order

1. Schema: `groups`, `group_members`, `permission_rules`, `effective_access`, `permission_rule_overrides`.
2. Host fn `permissions.can` (single check, bez cache).
3. Seed reguł R001-R007.
4. Integracja z addonami: każdy addon **musi** wołać `permissions.can` lub `permissions.list_for_user` w swoich listach (zero ręcznych check'ów).
5. Materialized cache `effective_access` + invalidation triggerów.
6. UI P2 — wyświetlenie reguł + macierzy grants.
7. UI P2 — wizard nowej reguły (kroki 1-5).
8. UI P4 Inspector (debug).
9. CRUD na grupach (UI).
10. Async recalculation z job queue.
11. Audit log + RODO compliance (kto kiedy widział co — zapisywać access log dla wrażliwych zasobów).
12. Migracja z IntrApp.

## Otwarte decyzje

1. **Inheritance reguł** — jeśli mam regułę „dyrektor sekcji widzi deale sekcji", to czy CEO automatycznie widzi też (bo jest wyżej)? Rekomendacja: **nie dziedzicz przez hierarchię — CEO ma własną regułę R005 `see_everything`**. Dziedziczenie powoduje pułapki (gdy ktoś nie chce widzieć).

2. **Performance dla 10k+ deali** — czy `effective_access` cache się skaluje? Dla małej firmy OK, dla dużej trzeba przejść na **policy evaluation runtime** (bez precompute) z indeksami na często sprawdzanych polach. Rekomendacja: **start z cache, monitoring; przełącz na runtime gdy >100k rekordów per addon**.

3. **Row-level security w PostgreSQL** — można delegować część check do RLS (PostgreSQL). Plus: szybko, bezpiecznie (zero shot na bypass). Minus: logika rozproszona (część w aplikacji, część w DB). Rekomendacja: **w MVP w aplikacji, RLS jako optymalizacja v2 dla dużych klientów**.

4. **Audit log per access (RODO)** — czy każdy odczyt wrażliwych danych logować? Rekomendacja: **tak, ale tylko dla resource_type oznaczonych jako sensitive** (admin może oznaczyć addon). Domyślnie tylko write/delete/approve.

5. **Field-level permissions** (np. „Dyrektor widzi deal ale nie widzi marży") — rekomendacja: **na MVP nie**. Dodać później przez `field_visibility_rules` jako extension nad `permission_rules`.
