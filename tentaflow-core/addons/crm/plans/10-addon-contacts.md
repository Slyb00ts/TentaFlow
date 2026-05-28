# 10 · Addon — Contacts

**Mockupy:** [K1 lista](../../../../../.gstack/projects/Slyb00ts-TentaFlow/designs/crm-v1/k01-contacts-list.html) · [K2 osoba](../../../../../.gstack/projects/Slyb00ts-TentaFlow/designs/crm-v1/k02-person-detail.html) · [K3 firma](../../../../../.gstack/projects/Slyb00ts-TentaFlow/designs/crm-v1/k03-company-detail.html) · [K4 mapa relacji](../../../../../.gstack/projects/Slyb00ts-TentaFlow/designs/crm-v1/k04-relationship-map.html)

## Cel

Centralna baza encji „kto / co" — firmy, osoby, relacje, grupy kapitałowe. Single source of truth dla wszystkich addonów (CRM, Calendar, Billing, Email). Inne addony nigdy nie duplikują kontaktów — tylko referują przez resource id.

W IntrApp było rozproszone (`Contacts`, `ContactAttributes`, `ContactPersons`, `ContactPersonAttributes`) — tutaj konsoliduje.

## Domeny i encje

### `companies`

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `name` | TEXT | Pełna nazwa prawna |
| `display_name` | TEXT | Skrócona do UI |
| `nip`, `regon`, `krs` | TEXT | Identyfikatory PL |
| `vat_id` | TEXT | EU VAT (dla zagranicy) |
| `address_street`, `address_city`, `address_postal`, `address_country` | | |
| `website`, `phone_main`, `email_main` | | |
| `industry` | TEXT | „Bankowość", „Retail", „Motoryzacja"… (z katalogu `industries`) |
| `size_employees` | INT | Liczba pracowników (orient.) |
| `parent_company_id` | UUID FK | Spółka-matka (dla grup kapitałowych) |
| `parent_share_pct` | DECIMAL | % udziałów matki w tej spółce |
| `is_active`, `created_at`, `updated_at` | | |
| `created_by` | UUID | Kto dodał |

### `persons`

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `first_name`, `last_name` | TEXT | |
| `email_primary` | TEXT | |
| `phone_primary` | TEXT | |
| `linkedin_url` | TEXT | |
| `kind` | ENUM | `internal | external | candidate` (internal = nasz pracownik z user account; external = klient/partner) |
| `user_id` | UUID FK | Dla `kind=internal` — link do TentaFlow user (auth) |
| `current_employer_company_id` | UUID FK | Tylko dla `kind=external` — firma w której obecnie pracuje |
| `current_position_in_company` | TEXT | „CIO", „Dyrektor IT" (free-form, nie z katalogu ról) |
| `language` | TEXT | „pl", „en" — do mailingów |
| `rodo_consent_at` | TIMESTAMP | Zgoda kontaktu (RODO) |
| `notes` | TEXT | Free-form |
| `is_active`, `created_at`, `updated_at` | | |

### `person_emails` / `person_phones` (multi)

| Pole | Typ | Opis |
|---|---|---|
| `person_id` | UUID FK | |
| `value` | TEXT | Adres / numer |
| `kind` | ENUM | `work | private | other` |
| `is_primary` | BOOL | |

### `company_persons` (osoba ↔ firma z historią)

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `person_id` | UUID FK | |
| `company_id` | UUID FK | |
| `position_title` | TEXT | „CIO", „Dyrektor IT" |
| `department` | TEXT | Free-form (po stronie klienta) |
| `started_at`, `ended_at` | DATE | Historia zatrudnienia (osoba mogła pracować w mBank, potem przejść do PKO) |
| `is_current` | BOOL | Bieżące zatrudnienie |
| `is_primary` | BOOL | Główna pozycja jeśli kilka równocześnie |

Multi-employment dozwolone (np. ktoś jest w board kilku spółek).

### `person_roles_in_sales` (rola external w sprzedaży)

| Pole | Typ | Opis |
|---|---|---|
| `person_id` | UUID FK | |
| `role_id` | UUID FK → [`roles`](./00-platform-roles-catalog.md) | z `kind=external` (Decydent / Influencer / Power user / Sponsor) |
| `default_for_company_id` | UUID FK | Opcj — domyślnie ta rola dla tej firmy |

Person może mieć kilka ról external (np. „Decydent" w jednym dealu, „Influencer" w innym — ale to są wpisy w `responsible_persons` deala, nie tutaj). Tutaj `person_roles_in_sales` to **default** dla danej osoby (co najczęściej robi).

### `tags`

Płaska tabela tagów (per właściciel/tenant): `id`, `name`, `color`. Łączona z `companies` i `persons` przez `company_tags` / `person_tags`.

### `lists` (smart lists)

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `name` | TEXT | „VIP top 50", „Bank&finanse", „Bez kontaktu >6mc" |
| `kind` | ENUM | `static | dynamic` |
| `query` | JSONB | Dla dynamic — predykat |
| `owner_user_id` | UUID FK | |
| `is_public` | BOOL | |

`static` → łączenie przez `list_members` table. `dynamic` → query rozwijany on the fly.

### `industries`, `company_groups`

Katalogi pomocnicze (admin CRUD lub seed).

## Lifecycle

**Osoba (external):**
```
created (manual / import / extracted-from-mail)
  → assigned to company (current employment)
  → assigned role(s) in sales (decydent / influencer / power user)
  → may change company over time
  → marked inactive (left market, retired) OR
  → merged with duplicate
```

**Firma:**
```
created (manual / import / NIP lookup from gov registry)
  → people assigned over time
  → linked to parent company (grupa kapitałowa)
  → tagged, listed
  → marked inactive (zlikwidowana)
```

Brak twardych przejść stage'owych — kontakty żyją „zawsze".

## UI surfaces

### K1 — Lista kontaktów

**Akcje:**

1. **Filtr** (chips top): typ (firma/osoba), branża, status (aktywny/nieaktywny), tag, aktywność (X dni temu). Filtry są URL-state — można podlinkować.
2. **Search box** — fuzzy po nazwie, NIP, email. Backend: trigram index PostgreSQL.
3. **Toggle widoku:** Tabela / Karty / Relacje (otwiera K4).
4. **"+ Nowy kontakt"** (top right) → modal wyboru: firma / osoba → odpowiedni formularz.
5. **Klik wiersza** → K2 (osoba) lub K3 (firma).
6. **Bulk selection + akcje:** masowy tag, masowy add-to-list, masowy export CSV.
7. **„Zapisz jako smart list"** (margin-left auto) — zapisuje aktualne filtry jako `lists` typu dynamic.

### K2 — Person detail (flagowy mockup wzorca contributions)

**Sekcje (top-down):**

1. **Header** — avatar, nazwa, rola (chips), aktualne stanowisko, dane kontaktowe, RODO chip, akcje (Email/Phone/„+ Akcja")
2. **AI insights bar** — 3 obserwacje z historii ("decydent w 2 wygranych", "preferuje pn-wt 18-21" etc.) — generowane przez `contacts.compute_person_insights` (tool AI)
3. **3-kolumnowy layout:**
   - Lewa: dane podstawowe + współpracownicy (z `company_persons` dla current employer)
   - Środek: unified timeline (agregat z activity addona — eventy ze wszystkich addonów dotyczące tej osoby)
   - Prawa: **contributions z innych addonów** przez view contracts:
     - „Aktywne deale" (z `crm.deals_for_contact`)
     - „Spotkania" (z `calendar.events_for_person`)
     - „Wątki mailowe" (z `email.threads_for_person`)
     - „Faktury / rozliczenia" (z `billing.invoices_for_person`)
     - „Dokumenty" (z `documents.files_for_resource`)

**Akcje:**

1. **„Edytuj"** (ikonka przy nazwie) → modal edycji
2. **„Email"** → wybór adresu (jeśli multi) + otwiera composer (z email addona)
3. **„Zadzwoń"** → kopiuje numer + opcj. integracja z VOIP
4. **„+ Akcja"** → menu kontekstowe (+ Deal / + Task / + Spotkanie / + Notatka)
5. **Inline edit** każdego pola w dane podstawowe — kliknięcie → input → blur saves
6. **Klik chip „decydent"** → modal edycji role_in_sales (z [00 katalog ról](./00-platform-roles-catalog.md))
7. **„+ Dodaj osobę" w sekcji „Aktywne deale"** → modal: który deal? z preset contact_id
8. **„Zaproponuj spotkanie"** → woła `calendar.propose_meeting(person_id)` (z [04 AI Broker](./04-platform-ai-tool-broker.md))

### K3 — Company detail

**Sekcje:**

1. **Header** — logo (initials), nazwa, NIP, adres, link do strony, link do grupy kapitałowej
2. **KPI:** aktywne deale, historia (wygrane), osoby kontaktowe, % zapłacone faktury
3. **Osoby kontaktowe** — tabela z role chips + ostatni kontakt
4. **Aktywne deale** + **historia (4 wygrane)** — z `crm.deals_for_company`
5. **Grupa kapitałowa** — visual hierarchy (parent + sister companies + subsidiaries)
6. **AI insights** — cykl decyzyjny, klucz relacji, potencjał cross-sell w grupie, konkurencja

### K4 — Mapa relacji

Spatial canvas (uzasadniony tylko TUTAJ — `account-based selling` map dla pojedynczej firmy/dealu).

Węzły = osoby (kolor po roli w sales: zielony decydent / fioletowy influencer / niebieski user / czerwony bloker / szary nieznany).
Krawędzie = hierarchia raportowania (z `company_persons` + dane z LinkedIn jeśli sync).

**Akcje:**
- Klik węzła → drill do K2 osoby
- Drag węzła → reorganizacja layoutu (cache w `lists.layout` jeśli zapisany jako saved view)
- „Sugerowane działania" panel (AI propozycje — „spotkaj się z CFO bo bloker budżetu")

## Provided contracts

**Resources:**
- `contact` (alias dla person + company — generic kontakt)
- `person`
- `company`

**Queries:**
- `contacts.search(query, kind?) → list<Contact>` — globalne wyszukiwanie
- `contacts.get_person(id) → Person`
- `contacts.get_company(id) → Company`
- `contacts.list_persons_in_company(company_id, current_only: bool) → list<Person>`
- `contacts.get_relations(person_id) → RelationGraph` — dla K4
- `contacts.list_for_smart_list(list_id) → list<Contact>` (zarówno static jak dynamic)

**Actions:**
- `contacts.create_person(input) → Person` (act, confirmation if AI)
- `contacts.update_person(id, input)`
- `contacts.create_company(input)` — z opcj. lookup z rejestru gov po NIP
- `contacts.merge_persons(primary_id, duplicate_id)` — deduplikacja (rzadko ręczne, częściej AI sugeruje)
- `contacts.attach_person_to_company(person_id, company_id, position_title, started_at)`
- `contacts.update_rodo_consent(person_id, consent_at)`

**Views:**
- contribution dla slotów `contact.detail.sidebar`, `contact.detail.main`, `company.detail.*`, `command_palette`, `global.search`

**Events:**
- `contacts.person_created/updated`, `contacts.company_created/updated`, `contacts.merged`, `contacts.rodo_consent_updated`

**AI Tools:**
- `contacts.find_or_create_person(name, company_name?, email?)` — draft (1-click), znajdź lub stwórz
- `contacts.find_or_create_company(name, nip?)` — draft
- `contacts.extract_from_text(text)` — read, wyciąga osoby/firmy z fragmentu maila/transkrypcji
- `contacts.compute_person_insights(person_id)` — read, generuje 3 obserwacje AI (decision pattern, preferowane godziny, historia)

## Consumed contracts (needs)

```toml
[needs.platform]
permissions = ["can", "list_for_user"]
roles = ["read"]
org = ["read"]

[needs.activity]
publish_event = ["contact_activity"]
read_timeline = ["for_resource"]  # do agregacji w K2 timeline
```

## Permissions

- **Read kontakty** — domyślnie wszyscy (R001 z [02](./02-platform-permissions.md))
- **Write kontakty** — handlowcy + sales-managers (`flag: can_edit_deal` + odpowiednia rola)
- **Merge** — tylko admin (destrukcyjne)
- **RODO consent update** — `flag: data_protection_officer` lub admin

## Migracja z IntrApp

| IntrApp | TentaFlow Contacts |
|---|---|
| `Contacts` + `ContactAttributes` | `companies` (gdy `IsCompany=true`) lub `persons` (gdy person) |
| `ContactPersons` + `ContactPersonAttributes` | `persons` (kind=internal jeśli ma login, external w przeciwnym razie) |
| `Phones` table | `person_phones` |
| `Emails` | `person_emails` |
| `ContactPersonAttributes.SectionId`, `DepartmentId`, `JobStaticId` | dla `kind=internal` → mapowane do [01 org structure](./01-platform-org-structure.md) (positions/assignments). Dla `kind=external` — pomijane (osoba klienta nie ma sekcji u nas) |
| `ProjectContactPersons.Title` (free-form rola u klienta) | `person_roles_in_sales.role_id` (z `kind=external`) jeśli matchuje katalog; w przeciwnym razie zapisane w `person_notes` lub jako `current_position_in_company` |

Skrypt migracji: deduplication po `nip` (firmy) i `email_primary` (osoby) — wszystkie dub instancje merge'owane.

## Implementation order

1. Schema: companies, persons, person_emails/phones, company_persons, person_roles_in_sales, tags, lists.
2. Host fn read: search, get_person, get_company, list_persons_in_company.
3. UI K1 lista (read-only).
4. Host fn write: create/update.
5. UI K1 — „+ Nowy kontakt" modal.
6. UI K2 person detail — header + dane podstawowe + inline edit.
7. View contributions registration (sidebar slots).
8. UI K3 company detail + grupa kapitałowa hierarchy.
9. NIP lookup (integration z rejestrami gov — opcj. później).
10. UI K4 relationship map — D3.js/cytoscape.
11. Smart lists CRUD + dynamic evaluation.
12. AI tools: find_or_create_*, extract_from_text, compute_person_insights.
13. Merge persons (z UI confirm + audit).
14. RODO consent workflow.
15. Migracja z IntrApp.

## Otwarte decyzje

1. **Czy `contact` to oddzielna encja czy tylko alias `person | company`?** Rekomendacja: **alias** — pewne UI (np. K1) treats them uniformly; baza ma osobne tabele.
2. **Deduplikacja automatyczna** — AI proponuje merge gdy znajdzie podobne wpisy. Może być irytujące (false positives) lub bardzo cenne. Rekomendacja: **AI generuje listę „prawdopodobnie ten sam" jako smart list, admin merge'uje ręcznie**.
3. **Sync z LinkedIn** — eksponuje dane osób. Płatne API. Rekomendacja: **opcj. integration v2, MVP bez**.
4. **Cross-tenant kontakty** — gdy ten sam człowiek (np. CIO mBanku) jest klientem dwóch tenantów TentaFlow, czy istnieje shared global registry? Rekomendacja: **na MVP per-tenant, multi-tenant kontakty w v3**.
5. **Free-form `current_position_in_company` vs katalog stanowisk** — po stronie klienta to free-form (CIO/CTO/CFO/Dyr Sprzedaży/…). Rekomendacja: **free-form z autocomplete na podstawie historii wpisów**.
