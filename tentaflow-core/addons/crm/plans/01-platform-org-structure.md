# 01 · Platform — Struktura organizacyjna

**Mockup:** [O1 Org Structure](../../../../../.gstack/projects/Slyb00ts-TentaFlow/designs/crm-v1/o01-org-structure.html)

## Cel

**Uniwersalne narzędzie platformy.** Modeluje organizację: **stanowiska** (positions), **przypisania osób do stanowisk**, **hierarchię raportowania**. Stanowisko referuje rolę z [katalogu ról](./00-platform-roles-catalog.md).

**Sama struktura jest addon-agnostyczna** — to czyste drzewo z host fn typu `org.is_subordinate_of`, `org.get_subordinates`, `org.get_reports_chain`. **Każdy addon sam decyduje** jak ją wykorzystuje (CRM do widoczności deali sekcji, Billing do approve chain budżetów, Activity do delegacji tasków, Calendar do team visibility itd.). Reguły uprawnień są w [02 Permissions](./02-platform-permissions.md), domyślne scope'y per rola w [00 Katalog ról](./00-platform-roles-catalog.md).

W IntrApp struktura była płaska (`Sections` bez `ParentId`). Tutaj **drzewo o dowolnej głębokości** + multi-podległość (matrix org).

## Domeny i encje

### `positions` (stanowiska — szkielet org)

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `name` | TEXT | Etykieta („Dyrektor sekcji Wschód") |
| `role_id` | UUID FK → [`roles`](./00-platform-roles-catalog.md) | Funkcjonalna rola tego stanowiska (rzutuje flagi/uprawnienia) |
| `department_id` | UUID FK | Dział (osobna encja, patrz niżej) |
| `section_id` | UUID FK | Sekcja (osobna encja) — opcjonalna |
| `is_active` | BOOL | |
| `created_at`, `updated_at` | TIMESTAMP | |
| `meta` | JSONB | Dowolne dodatkowe pola dla danej organizacji |

### `position_reports_to` (hierarchia — multi-parent)

| Pole | Typ | Opis |
|---|---|---|
| `position_id` | UUID FK | Stanowisko podległe |
| `parent_position_id` | UUID FK | Stanowisko nadrzędne |
| `priority` | INT | 1 = główny przełożony, 2+ = wtórny (matrix org) |

PK: `(position_id, parent_position_id)`. Jedno stanowisko może mieć wielu rodziców (matrix), ale tylko jeden z `priority=1`.

### `position_assignments` (osoba ← stanowisko, z historią)

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `position_id` | UUID FK | |
| `person_id` | UUID FK → [`contacts.persons`](./10-addon-contacts.md) | |
| `assigned_at` | DATE | Od kiedy |
| `terminated_at` | DATE | Do kiedy (null = aktywne) |
| `assignment_type` | ENUM | `permanent | acting | external_contractor` |
| `share` | DECIMAL | % etatu w tym stanowisku (1.0 = pełen) |

Pozwala: osoba może obejmować dwa stanowiska na pół etatu; gdy zmieni stanowisko, stare `terminated_at = today` i nowe `assigned_at = today` (historia zostaje).

### `departments` (działy)

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `name` | TEXT | „Sprzedaż", „Realizacja", „Administracja" |
| `parent_department_id` | UUID FK | Działy mogą być zagnieżdżone (np. „Sprzedaż → Sprzedaż B2B → Sprzedaż enterprise") |
| `code` | TEXT | Krótki kod (do raportów) |

### `sections` (sekcje — bardziej szczegółowe grupy w dziale)

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `name` | TEXT | „Sekcja Wschód", „Sekcja Centrala" |
| `department_id` | UUID FK | |
| `is_technical` | BOOL | Filtr dla niektórych UI (sprzedaż vs realizacja) |

## UI surfaces

### O1 Drzewo — canvas z zoom/pan (główny widok)

**Wymagania UX kluczowe (nie były spełnione w pierwszej wersji mockupu, są teraz):**

- **Prawdziwe drzewo na canvas SVG** (nie ułożone divy). Węzły = stanowiska, krawędzie = linie raportowania.
- **Zoom** (kółko myszy lub `+`/`−` w toolbar) — transform `scale()` na grupie SVG. Zakres np. 30% – 200%.
- **Pan** (drag tła) — transform `translate()`. Cursor `grab` → `grabbing`.
- **Drag węzła** — przeciąganie zmienia `position_reports_to.parent_position_id`.
- **Minimapa** w prawym dolnym rogu — pełna struktura w skali, viewport zaznaczony prostokątem. Klik w minimapę → przesuwa viewport.
- **Toolbar** (lewy górny róg): zoom −/+ z zoom-level, Fit-to-screen, układ Top-Down / Left-Right / Radial, tryb Edycja, Zwiń/Rozwiń wszystko.
- **Hint** (lewy dolny róg): `scroll` zoom · `drag` pan · `drag węzeł` reorganizuj · `⌘+klik` matrix-parent.
- **Linie raportowania:** ciągła = główny przełożony (priority=1), kropkowana (`stroke-dasharray`) = drugi przełożony (matrix, priority=2).
- **Vacancy** = węzeł z przerywaną ramką + tekst „— wakat —".
- **Collapse podwładnych** — kółko z `+N` na końcu rozwiniętego węzła, klik rozwija/zwija.

**Akcje:**

1. **Klik węzła** → prawa kolumna pokazuje detail stanowiska:
   - Nazwa stanowiska + rola (z katalogu)
   - Osoba (z `position_assignments` aktywne) lub vacancy
   - Hierarchia (raportuje do, drugi przełożony, bezpośredni podwładni, transitive count)
   - **„Korzystają z hierarchii"** — sekcja generic: lista addonów które konsumują strukturę (CRM, Billing, Activity, Calendar) z chipem jakie host fn wołają. Link do P2 dla edycji reguł. **Nie ma tu hardcoded reguł widoczności** — to addon-specific concern.
   - Akcje: Edytuj / Zmień osobę / Dodaj podwładnego / Dodaj matrix-parent / Historia

2. **Drag węzła** (w trybie Edycja) → zmienia parent
   - Walidacja: cykl forbidden
   - Konfirm: „Zmieniasz przełożonego dla N podwładnych w dół. OK?"
   - Po zapisie: emit `org.position_moved` z diffem (z payloadem `affected_subtree`)
   - **Każdy konsumujący addon sam reaguje na ten event** — niczego nie wywołujemy w org tool. Permission engine i tak periodycznie recalculatuje effective_access, ale to też nie nasza odpowiedzialność (z [02](./02-platform-permissions.md)).

3. **„+ Nowe stanowisko"** (top right)
   - Modal: nazwa, rola, dział, sekcja, parent position
   - INSERT `positions` + `position_reports_to`
   - Event: `org.position_created`

4. **„Przypisz osobę"** (na vacancy)
   - Modal: select z `contacts.persons` (kind=internal)
   - Pola: assigned_at, share, assignment_type
   - INSERT `position_assignments`
   - Event: `org.person_assigned`

5. **„Zwolnij stanowisko"**
   - Modal potwierdzenia
   - UPDATE `position_assignments` SET terminated_at = today
   - Event: `org.person_terminated`

6. **Filtry** (chips nad canvas): per dział, „tylko wakaty", toggle „pokaż role", „pokaż awatary".

### O1 Lista (zakładka)

Tabela: stanowisko, rola, dział, osoba, manager, ostatnia zmiana. Filtry, sortowanie, eksport CSV. Działania jak w drzewie (kontekstowe menu na wierszu).

### O1 Katalog ról (zakładka)

Linkuje do [O2 mockup](../../../../../.gstack/projects/Slyb00ts-TentaFlow/designs/crm-v1/o02-roles-catalog.html) — patrz [00 plan katalogu ról](./00-platform-roles-catalog.md).

### O1 Historia zmian (zakładka)

Audit log struktury z diffami. Niezbędne do RODO (kto kiedy miał jaką pozycję — implikuje kto kiedy miał dostęp do czego, ale **same reguły dostępu są w P2**, nie tutaj).

### Co świadomie wyrzucone z tego mockupu

Pierwsza wersja miała sekcję „Widoczność danych (auto)" w prawym sidebar. **Wyrzucone**, bo:
- To wprowadza addon-specific knowledge do narzędzia które ma być uniwersalne
- Reguły uprawnień są w `permission_rules` ([02](./02-platform-permissions.md)) — admin łączy strukturę z grupami i regułami per addon
- Domyślne scope'y per rola są w [00 Katalog ról](./00-platform-roles-catalog.md)
- Debugger „kto co widzi" jest w **P4 Effective Access Inspector** (planowany — nie ma jeszcze mockupu)

Org tool tylko **publikuje strukturę i eventy** o zmianach — co z tego wynika dla widoczności addonów jest decyzją addonów + admin reguł.

### Edytor stanowiska (modal)

Pola:
- Nazwa
- Rola (select z O2)
- Dział + sekcja
- Parent positions (główny + opcj. drugi dla matrix)
- Aktywne (toggle)
- Notatki

Walidacja:
- Nazwa unique w obrębie sekcji
- Brak cyklu w parent chain
- Rola musi istnieć i być aktywna

## Provided contracts

**Resources:**
- `position` (PositionProvider)
- `department` 
- `section`
- `person_assignment`

**Host functions:**
- `org.list_positions(filter: {department?, section?, role_kind?, vacant?})`
- `org.get_position(id)`
- `org.get_assignment(person_id, at: date?)` — kto kim jest dziś (lub w dacie)
- `org.get_reports_chain(position_id, direction: up|down) → list<Position>` — łańcuch przełożonych lub podwładnych
- `org.get_subordinates(position_id, transitive: bool)` — bezpośredni lub wszyscy w dół
- `org.is_subordinate_of(person_id, manager_position_id) → bool` — pomocna dla permissions
- `org.create_position(input)`
- `org.assign_person(position_id, person_id, ...)`
- `org.terminate_assignment(assignment_id, terminated_at)`
- `org.move_position(position_id, new_parent_id)` — z walidacją cyklu

**Events:**
- `org.position_created`
- `org.position_moved` (zawiera diff parent)
- `org.person_assigned`
- `org.person_terminated`
- `org.position_deactivated`

**Read przez wszystkich** (każdy addon musi widzieć org żeby liczyć permissions), **Write tylko admin**.

## Permissions

**Sztywna zasada platformy: tylko admin pisze. Brak delegacji.** User i Power User mają tylko read.

- **Read pełna struktura** — wszyscy zalogowani (każdy musi widzieć drzewo żeby wiedzieć kto jest jego managerem, kto podlega mu)
- **Write** (`org.create_position`, `org.move_position`, `org.assign_person`, `org.terminate_assignment`, `org.deactivate_position`) — **wyłącznie `system.admin`**
- **NIE MA** delegacji typu „dyrektor edytuje swój dział". To była pokusa w pierwszej wersji planu — wyrzucone. Jeśli admin chce delegować, robi to ręcznie albo przekazuje admin role.

UI dla non-admin:
- Drzewo widoczne **read-only** — brak drag-drop, brak buttonów „+ Stanowisko" / „Przypisz" / „Zwolnij"
- Klik węzła pokazuje detail w sidebar (bez „Edytuj" buttona)
- Topbar bez „Zapisz" buttona; zamiast tego info „Tylko podgląd — edytuje admin"

Backend wymusza to przez `require_admin(ctx)` w handlerach write. Jeśli non-admin spróbuje przez API → `PolicyDenied("admin required")`.

Ta sama zasada obowiązuje dla:
- **Katalog ról** ([00](./00-platform-roles-catalog.md)) — write tylko admin
- **Permissions** ([02](./02-platform-permissions.md)) — write tylko admin
- **AI Tools registry** ([04](./04-platform-ai-tool-broker.md)) — write tylko admin
- **App settings / Resource grants** — write tylko admin

## Migracja z IntrApp

Z IntrApp ciągniemy:
- **Departments** → jeden do jednego (`name`, `parent_department_id` = null bo IntrApp ma płasko, można poukładać ręcznie)
- **Sections** → `sections` z `is_technical` = `IsTechnical`
- **Sections.ManagerId** → konwertuje się na: stworzenie position „Dyrektor sekcji X" z `role_id` = `section_director` i przypisaniem osoby z `ManagerId`
- **ContactPersonAttributes.SectionId / DepartmentId / JobStaticId / Job** → tworzymy `positions` per pracownik (1:1 osoba ↔ stanowisko, jeśli ktoś miał wiele, robimy 2 positions)
- **Jobs** (tabela ról IntrAppowych) → już zmigrowane przez [00 katalog ról](./00-platform-roles-catalog.md)
- **Sections.ManagerId** + **Jobs.IsManager** → łącznie wyznaczają hierarchię, ale w IntrApp nie ma jawnego drzewa. Musimy je zbudować ręcznie podczas migracji (admin pociąga raz, dalej edytuje w O1).

Pomijamy:
- `UserGroup.Superiors` — w IntrApp było luźne pojęcie „kto jest przełożonym grupy". W TentaFlow grupa to RBAC, nie hierarchia. Hierarchia idzie przez `position_reports_to`.

## Implementation order

1. Schema: `positions` + `position_reports_to` + `position_assignments` + `departments` + `sections`.
2. Host fn (read-only): `org.list_*`, `org.get_*`, `org.is_subordinate_of`, `org.get_reports_chain`, `org.get_subordinates(transitive)`.
3. UI O1 Drzewo — read-only canvas SVG z mock data:
   - Komponent `tf-org-tree` z transform-based zoom/pan
   - Layout engine (Reingold–Tilford albo prostszy — drzewo top-down)
   - Minimapa (osobny SVG z viewport rect)
4. UI O1 Drzewo — interakcja: zoom (scroll), pan (drag tła), select węzła
5. Host fn (write): `org.create_position`, `org.assign_person`, `org.terminate_assignment`, `org.move_position`
6. UI O1 — modale „+ Stanowisko", „Przypisz osobę", drag-drop reorganizacji + walidacja cyklu
7. Event publishing — addony subskrybują same, my nie obchodzimy ich consekwencji
8. UI O1 Lista (zakładka) + Historia zmian
9. UI sidebar prawej kolumny — generic detail (bez visibility hints — to wyrzucone)
10. Audit log na wszystkie write
11. Layout alternatives — Left-Right + Radial (opcja v2)
12. Migracja z IntrApp (skrypt importujący Departments/Sections/ContactPersonAttributes — patrz sekcja migracji)

## Otwarte decyzje

1. **Czy multi-parent (matrix org) jest must-have?** Rekomendacja: **schema gotowa na matrix od dnia 1, UI pokazuje priority=1 jako linię ciągłą, priority=2 jako linię kropkowaną. Brak „advanced" trybu — wszystko widoczne od razu.**

2. **Czy struktura ma być versionowana (kto był dyrektorem rok temu)?** `position_assignments` daje historię osób, ale nie historię struktury (kto był parent X dwa lata temu). Rekomendacja: **dodać `position_reports_to_history` jako audit table, wypełnia się triggerem przy każdej zmianie**. Na MVP tylko `audit_log` w warstwie aplikacji.

3. **Czy stanowisko może być „zewnętrzne" (kontraktor)?** Tak — `assignment_type = external_contractor`. Daje to wskazówkę do widoczności (kontraktor widzi tylko swój projekt, nie sekcję).

4. **Drzewo zatwierdzania kart akceptacji** — czy zatwierdzający dla kart >500k to **dyrektor sekcji** czy **dyrektor handlowy**? Powinno wynikać z flag roli (`can_approve_budget.threshold_pln`). Karta automatycznie idzie do najniższego stanowiska w łańcuchu raportowania który ma flagę z odpowiednim progiem.
