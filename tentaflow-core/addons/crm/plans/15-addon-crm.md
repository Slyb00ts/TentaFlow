# 15 · Addon — CRM (rdzeń sprzedażowy)

**Mockupy:** [C1 Daily Inbox](../../../../../.gstack/projects/Slyb00ts-TentaFlow/designs/crm-v1/c01-daily-inbox.html) · [C2 Pipeline](../../../../../.gstack/projects/Slyb00ts-TentaFlow/designs/crm-v1/c02-pipeline.html) · [C3 Deal detail](../../../../../.gstack/projects/Slyb00ts-TentaFlow/designs/crm-v1/c03-deal-detail.html) · [C4 Deal create AI](../../../../../.gstack/projects/Slyb00ts-TentaFlow/designs/crm-v1/c04-deal-create.html) · [C5/C6/C7 dashboardy] · [C8 Forecast](../../../../../.gstack/projects/Slyb00ts-TentaFlow/designs/crm-v1/c08-forecast.html) · [C9 Karta akceptacji](../../../../../.gstack/projects/Slyb00ts-TentaFlow/designs/crm-v1/c09-acceptance-card.html) · [C10 Cost refactor](../../../../../.gstack/projects/Slyb00ts-TentaFlow/designs/crm-v1/c10-cost-refactor.html) · [C11 AI chat](../../../../../.gstack/projects/Slyb00ts-TentaFlow/designs/crm-v1/c11-ai-chat.html) · [C12 Smart inbox](../../../../../.gstack/projects/Slyb00ts-TentaFlow/designs/crm-v1/c12-smart-inbox.html) · [C13 Palette](../../../../../.gstack/projects/Slyb00ts-TentaFlow/designs/crm-v1/c13-command-palette.html) · [C14 Stages](../../../../../.gstack/projects/Slyb00ts-TentaFlow/designs/crm-v1/c14-stages-settings.html)

## Cel

Rdzeń sprzedażowy. Cykl życia deala (Lead → Oferta → Commit → Realizacja → Won/Lost), forecast, karty akceptacji budżetu, refakturowanie kosztów (w cooperacji z [Billing](./13-addon-billing.md)). **Centrum operacyjne handlowca** — Daily Sales Inbox to pierwszy ekran który handlowiec widzi otwierając TentaFlow.

Filozofia (z [PLAN.md](../PLAN.md) i konsultacji z codex):
1. **Inbox > baza danych** — pierwszy ekran to lista akcji na dziś, nie lista deali
2. **AI cichy administrator** — sygnały (`suggest`/`draft`/`act`) wpadają do tej samej kolejki co eventy biznesowe
3. **Jeden silnik widgetów dla wszystkich ról** — handlowiec, dyrektor, zarząd na różnych query, nie 3 osobne ekrany
4. **UI mówi „deal/oferta"** — słowo „projekt" pojawia się dopiero w fazie Realizacja

## Domeny i encje

### `deals` (rdzeń)

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `code` | TEXT UNIQUE | `DEAL-2026-0184` — auto-generowany |
| `name` | TEXT | „VW Poznań · Wdrożenie ERP fleet" |
| `description` | TEXT | Pełny opis |
| `contract_number` | TEXT | Numer umowy (z klientem) |
| `client_company_id` | UUID FK → contacts.companies | Mocodawca |
| `end_client_company_id` | UUID FK | Klient końcowy (opcj. — gdy mocodawca to nie ten sam) |
| `stage` | ENUM | `lead | qualifying | offer | commit | realization | won | lost` |
| `previous_stage` | ENUM | Dla śledzenia regresów |
| `lost_reason` | TEXT | Wymagany przy stage=lost |
| `value_pln` | DECIMAL | Wartość netto kontraktu w PLN |
| `value_currency` | TEXT | Oryginalna waluta |
| `value_original` | DECIMAL | Wartość w walucie oryginalnej |
| `exchange_rate` | DECIMAL | |
| `exchange_rate_date` | DATE | |
| `vat_rate_id` | UUID FK | |
| `account_id` | UUID FK | Konto księgowe |
| `margin_predicted_pct` | DECIMAL | Marża prognoz. |
| `margin_predicted_pln` | DECIMAL | Computed: value_pln × margin_predicted_pct |
| `planned_budget_pln` | DECIMAL | |
| `est_close_date` | DATE | |
| `realization_date` | DATE | Faktyczna (gdy stage=won) |
| `sign_date` | DATE | Podpis umowy |
| `end_date` | DATE | Planowane zakończenie wdrożenia |
| `duration_days` | INT | Czas trwania |
| `contract_kind_id` | UUID FK | Z katalogu (FixedPrice/T&M/Subscription itp.) |
| `commit` | BOOL | Zatwierdzony budżet (wpływa na forecast zarządu) |
| `probability_pct` | INT | Prawdopodobieństwo close (z catalog stage'a — może być overridden per deal) |
| `section_id` | UUID FK → org.sections | Sekcja sprzedażowa |
| `owner_user_id` | UUID FK | Handlowiec prowadzący |
| `pipeline_id` | UUID FK | Który pipeline (jeśli >1) |
| `acceptance_card_status` | ENUM | `none | sent | accepted | declined | expired` |
| `acceptance_card_file_id` | UUID FK → documents.files | |
| `acceptance_card_sent_at`, `accepted_at`, `decided_by_person_id` | | |
| `escalation_complete` | BOOL | Flaga gotowości do promocji do realizacji (po akceptacji karty) |
| `realization_status` | TEXT | Sub-status dla stage=realization („w toku 67%") |
| `billing_status_id` | UUID FK | Status fakturowania |
| `interval_id` | UUID FK | Interwał fakturowania (informacyjny — generator nie istnieje) |
| `risks` | JSONB | Lista wpisów risk: `[{label, severity, source}]` |
| `tags` | TEXT[] | |
| `is_active` | BOOL | Soft delete |
| `created_by`, `created_at`, `updated_at` | | |

### `deal_responsible_persons` (kluczowa — kto pracuje na deal)

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `deal_id` | UUID FK | |
| `person_id` | UUID FK → contacts.persons | |
| `role_id` | UUID FK → [`roles`](./00-platform-roles-catalog.md) | Z katalogu |
| `is_leader` | BOOL | Główna osoba w tej roli |
| `can_edit` | BOOL | Override flag z roli (zazwyczaj dziedziczone) |
| `send_notifications` | BOOL | Czy dostaje powiadomienia o tym dealu |
| `share_pct` | DECIMAL | % udział (do prowizji) |
| `joined_at`, `left_at` | DATE | Historia |
| `from_phase` | ENUM | W której fazie się przyłączył (np. PM dołącza od „commit") |

Identyczna z `responsible_persons` z innych addonów. Tutaj kluczowa dla widoczności (`assigned_to_self` predicate) i dla AI insights.

### `deal_client_contacts` (osoby po stronie klienta)

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `deal_id` | UUID FK | |
| `person_id` | UUID FK → contacts.persons | (z kind=external) |
| `role_id` | UUID FK | Z katalogu, kind=external (Decydent/Influencer/Sponsor itp.) |
| `is_primary` | BOOL | Główny kontakt |

### `pipeline_stages` (administrowalny pipeline)

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `pipeline_id` | UUID FK | Można mieć >1 pipeline (np. „Standard B2B" vs „Pilot programs") |
| `slug` | TEXT | `lead | qualifying | offer | commit | realization | won | lost` (default) |
| `name_pl`, `name_en` | TEXT | |
| `order` | INT | |
| `probability_pct` | INT | |
| `color_hint` | TEXT | |
| `required_fields` | TEXT[] | Pola wymagane przed przejściem dalej (np. dla `commit`: `commit=true`, `acceptance_card_accepted=true`) |
| `automations` | JSONB | Lista automatyzacji: `[{trigger, action}]` |
| `is_terminal` | BOOL | won/lost = true |

### `acceptance_cards`

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `deal_id` | UUID FK | |
| `file_id` | UUID FK → documents.files | Wygenerowany PDF |
| `budget_pln` | DECIMAL | Snapshot wartości w momencie wystawienia |
| `valid_days` | INT | Default 7 |
| `valid_until` | TIMESTAMP | |
| `approver_person_id` | UUID FK | Z org structure (osoba mająca flag `can_approve_budget` z threshold ≥ budget_pln) |
| `sent_at` | TIMESTAMP | |
| `link_accept_token` | TEXT | HMAC token w URL (jednorazowy) |
| `link_decline_token` | TEXT | |
| `link_view_token` | TEXT | |
| `decided_at` | TIMESTAMP | |
| `decision` | ENUM | `accepted | declined | expired` |
| `decision_metadata` | JSONB | IP, user-agent, device gdy klikał (RODO) |

### `trade_costs` (plan) / `trade_realization_costs` (real)

Te same kolumny, różne tabele.

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `deal_id` | UUID FK | |
| `category` | ENUM | `materials | external_services | internal_services | allowances | other` |
| `code` | TEXT | Auto-generowany |
| `description` | TEXT | |
| `quantity` | DECIMAL | |
| `unit_cost_pln` | DECIMAL | |
| `total_pln` | DECIMAL | Computed |
| `supplier_company_id` | UUID FK | Opcj. (jeśli wiadomo dostawca) |
| `expected_date` | DATE | |
| `created_at` | | |

Przy promocji do `stage=realization` (gdy `escalation_complete=true`):
1. Wszystkie `trade_costs` dla tego deala kopiowane do `trade_realization_costs` (z nowymi `code`)
2. `project_incomes` (plan przychodów) — analogicznie do `project_realization_incomes`
3. Mapowanie `linked_costs[]` w incomes — remapowane na nowe IDs

### `project_incomes` (plan) / `project_realization_incomes` (real)

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `deal_id` | UUID FK | |
| `code` | TEXT | Auto-gen |
| `name` | TEXT | „Faktura za fazę 1" |
| `invoice_number` | TEXT | **Numer faktury — punkt styku z billing/KSeF** |
| `value_pln` | DECIMAL | |
| `value_currency` | TEXT | |
| `value_original` | DECIMAL | |
| `exchange_rate` | DECIMAL | |
| `exchange_rate_date` | DATE | |
| `vat_rate_id` | UUID FK | |
| `expected_date` | DATE | |
| `realization_days` | INT | |
| `to_pay_days` | INT | |
| `linked_cost_ids` | UUID[] | Refakturowanie (które koszty pokrywa to przychód) |
| `linked_cost_document_id` | UUID FK → billing.cost_documents | Bezpośredni link gdy refaktura |

### `forecast_snapshots` (daily zatrzaśnięty forecast)

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `snapshot_date` | DATE | |
| `scope_kind` | ENUM | `user | section | department | org` |
| `scope_id` | UUID | Komu dotyczy |
| `period` | TEXT | „2026-05" / „2026-Q2" / „2026" |
| `target_pln` | DECIMAL | Cel |
| `commit_pln` | DECIMAL | Suma deali commit |
| `best_case_pln` | DECIMAL | + offer × probability + qualifying × probability |
| `worst_case_pln` | DECIMAL | commit − risk-flagged deals |
| `risk_pln` | DECIMAL | |
| `won_pln_to_date` | DECIMAL | |
| `created_at` | | |

### `deal_history` (audit kluczowych zmian)

| Pole | Typ | Opis |
|---|---|---|
| `deal_id` | UUID FK | |
| `event` | ENUM | `created | stage_changed | value_changed | commit_changed | acceptance_card_decided | promoted_to_realization | won | lost` |
| `payload` | JSONB | Diff |
| `actor_user_id` | UUID FK | |
| `at` | TIMESTAMP | |

(Część duplikuje się z `activity.timeline_events` — ale `deal_history` jest **lokalna w CRM** dla szybkiego dostępu w deal detail bez query do innego addona; dodatkowo `timeline_events` jest publikowany do globalnego strumienia.)

## Lifecycle deala (state machine)

```
[brak] --create--> Lead
   |
   v
Lead --qualify--> Qualifying
   |
   v
Qualifying --send_offer--> Offer
   |
   v                     <-- offer can regress back if rejected
Offer --commit_budget--> Commit  (wymagane: commit=true + acceptance_card_accepted)
   |
   v
Commit --escalation_complete=true--> Realization
   |    (clone plan→realization)
   v
Realization --finish--> Won (set realization_date, value materialized)
   |
   +--lose--> Lost (set lost_reason)
   
W każdej fazie można cofnąć (regres) — wyzwala signal w activity „deal się sypie".
```

### Magic values → enum

W IntrApp `status_list_status=100` = realizacja. Tutaj **`stage = "realization"`** — enum, czytelne.

### Promocja do realizacji (transakcja)

```
1. Walidacja: stage=commit AND escalation_complete=true AND acceptance_card_accepted=true
2. Start transaction:
   a. UPDATE deals SET stage='realization', previous_stage='commit', realization_status='in_progress'
   b. INSERT INTO trade_realization_costs SELECT * FROM trade_costs WHERE deal_id=X 
      (z nowymi code, mapowanie old_id → new_id w temp table)
   c. INSERT INTO project_realization_incomes SELECT *, linked_cost_ids=remap(linked_cost_ids) FROM project_incomes ...
   d. Emit event 'deal.promoted_to_realization' (z payloadem listy nowych IDs)
   e. Tworzy signal `meeting_aftermath` w activity dla owner deala („dodaj PM techniczny do zespołu")
3. Commit transaction
```

## UI surfaces — szczegółowy opis akcji

### C1 — Daily Sales Inbox

**Cel:** główny ekran handlowca. Adaptacyjna kolejka zdarzeń do decyzji.

**Komponenty:**

1. **Pasek celu (top)** — pokazuje cel miesięczny, realizacja, luka, commit forecast. Progress bar. Update'owany on event `deal.won/lost/commit_changed/value_changed`.

2. **Filtry chips (top filter row):**
   - „Wymaga decyzji" (default active) — wszystkie nieobsłużone signals + tasks z due ≤ dziś
   - „Odpowiedzi klientów" — eventy `email.received` linked do deali tego usera
   - „Po spotkaniu" — eventy `calendar.meeting_ended` w ostatnich 24h
   - „Sygnały AI" — signals z source=ai:*
   - „Stale" — signals z kind=stale_commit / offer_no_reply / margin_drop
   - Sort: priorytet (severity DESC, due_at ASC)

3. **Inbox rows** — każdy row to:
   - Ikona po lewej (typ eventu/signala — fire/mail/calendar/check itp.)
   - Tytuł + chip stage/severity
   - Sub-text (kontekst — kto/co/ile)
   - Meta (firma · osoba · czas)
   - Action buttons po prawej (1-3 akcje, zawsze pierwsza jest „polecana")

4. **Sekcja „Załatwione dziś"** (na dole, collapsable) — co user już zrobił dziś.

**Każdy typ row ma specyficzne akcje. Pełna lista:**

#### Row type: `stale_commit` (commit bez aktywności)
- **„Zadzwoń"** (primary) → otwiera modal call: kopiuje numer telefon kontaktu głównego deala, opcjonalnie startuje voip → po zakończeniu woła `activity.create_task` z opisem rozmowy
- **„Zdejmij commit"** → `crm.update_deal(deal_id, {commit: false})` (act z confirm — pokazuje wpływ na forecast)
- **„Odłóż"** → `activity.snooze_signal(signal_id, until=jutro 9:00)`

#### Row type: `email_replied` (klient odpisał)
- **„Wyślij draft AI"** (primary) — **TO JEST KLUCZOWA AKCJA — pełny flow:**
  ```
  1. Klik wyzwala: ai_broker.invoke_tool('email.draft_followup', {
       email_thread_id, deal_id, response_intent: 'auto-detect'
     })
  2. Tool wewnątrz email addona:
     a. Wczytuje thread + ostatnie 5 wiadomości
     b. Wczytuje deal context (stage, owner, status karty, ostatnie ustalenia)
     c. Wczytuje persona klienta z contacts insights
     d. LLM call z prompts: "Wygeneruj odpowiedź uwzględniającą tone klienta, fazę deala i ostatnie ustalenia"
     e. Zwraca {subject, body, attachments_suggestion}
  3. Pokazuje modal preview (nie confirm — to draft):
     - Editable subject + body
     - Suggested attachments (linki do plików z documents)
     - Buttony: [Wyślij teraz] [Edytuj] [Pomiń]
  4. Klik „Wyślij teraz" wyzwala (osobny tool, this time act):
     ai_broker.invoke_tool('email.send', {to, subject, body, attachments, thread_id, deal_id})
     - Tool sprawdza grants → wysyła przez SMTP (z konta usera) → zapisuje w emails
     - Emit event email.sent
     - Auto-link maila do deala
  5. Inbox row znika (signal resolved bo ostatnia odpowiedź jest od nas)
  ```
- **„Otwórz wątek"** → przejście do email addona, thread view
- **„Odłóż na 1h"**

#### Row type: `meeting_aftermath` (po spotkaniu)
- **„Update deala (diff)"** (primary) — workflow:
  ```
  1. ai_broker.invoke_tool('crm.update_deal_from_context', {
       deal_id, source: {kind: 'meeting', meeting_id: ...}
     })
  2. Tool:
     a. Wczytuje transcript ze spotkania (calendar.get_event + transcript_file_id)
     b. Wczytuje obecny stan deala
     c. LLM wyciąga: nowe wymagania, obiekcje, zmiany wartości, ryzyka, next steps
     d. Zwraca proposed diff: {risks_add: [...], requirements_add: [...], value_change?: ...}
  3. Confirm dialog z diffem (każda zmiana zaznaczalna checkboxem)
  4. User akceptuje wybrane → tool faktycznie wykonuje update na dealu
  5. Emit event deal.updated_from_meeting
  ```
- **„Draft follow-up"** → email.draft_followup analogicznie
- **„Otwórz spotkanie"** → calendar event detail

#### Row type: `acceptance_card_decided` (karta zatwierdzona)
- **„Promuj do realizacji"** (primary) — workflow:
  ```
  1. ai_broker.invoke_tool('crm.promote_to_realization', {deal_id}) — act, required confirm
  2. Walidacja (acceptance_card_accepted=true)
  3. Confirm dialog: pokazuje co zostanie sklonowane (N kosztów planowych → realizacyjne)
  4. Akceptuje → transakcja (patrz lifecycle wyżej)
  5. Inbox row znika, signal resolved
  ```
- **„Podgląd karty"** → modal z PDF viewer

#### Row type: `cost_pending_refacture`
- **„Refakturuj na deal"** → otwiera C10 (Cost refactor) z prefilled
- **„Pomiń (koszt własny)"** → `billing.mark_own_cost(cost_id)`

#### Row type: `upcoming_meeting`
- **„Otwórz brief AI"** → `calendar.generate_meeting_brief(event_id)` → modal z briefem
- **„Detale spotkania"** → calendar event detail

#### Row type: `offer_no_reply` / inne signale AI hygiene
- **„Nudge mailem (AI)"** → email.draft_followup z preset intent=nudge
- **„Zadzwoń"** → call workflow
- **„Oznacz jako zimny"** → ustawia tag `cold` na dealu + signal dismiss

### C2 — Pipeline kanban

**Cel:** widok pełnego pipeline'u sekcji. Drugorzędny po C1.

**Akcje:**

1. **Drag-drop deala między kolumnami** → workflow:
   ```
   1. Wstrzymanie drop, walidacja required_fields docelowego stage'a
   2. Jeśli OK: ai_broker.invoke_tool('crm.move_stage', {deal_id, new_stage, reason?})
   3. Confirm dialog jeśli nowa faza ma side-effects (np. commit=true wymaga karty)
   4. Update + emit event
   ```

2. **Filtry top:** sekcja, owner, wartość, tylko commit, stale > 7d, z ryzykiem AI

3. **Klik karty** → C3 deal detail

4. **„Nowy deal"** (top right) → C4 modal

5. **Toggle:** Lista (tabela) / Kanban (default) / Funnel (waterfall)

### C3 — Deal detail (najważniejszy widok roboczy)

**Layout:** 3-kolumnowy.

**Header:**
- Avatar firmy, nazwa, stage chip, commit chip, ID kodu deala, data utworzenia
- Po prawej: wartość, marża, prawd. close
- Buttons: Email / Zadzwoń / Promuj fazę
- Pasek stage progression (klikalny — można cofać/awansować z confirm)

**AI insights bar** (pod headerem):
- 3 obserwacje wygenerowane przez `crm.compute_deal_insights` (read tool, cache 5min)
- „Co dalej?" → otwiera C11 chat sidebar

**Lewa kolumna (Fakty):**
- Wartość (inline edit)
- Marża (inline edit)
- Est. close (inline edit z date picker)
- Prawd. close % (inline + progress bar)
- Commit toggle (with warning gdy zmiana)
- Owner (avatar — możliwa zmiana z confirm)
- Sekcja (chip — z org structure)
- Tagi (chips, dodawanie/usuwanie)
- **Next action** (kotlet specjalny, różowy):
  - „Zadzwoń teraz" → call workflow
  - Możliwość edycji next action (woła activity.create_task z aktualizacją)

**Środek (Aktywność = unified timeline):**
- Wzięta z `activity.read_timeline(linked_resource: {deal, deal_id})`
- Filtry: Wszystko / Maile / Spotkania / Notatki / Zmiany
- Każdy item ma `contrib-tag` wskazujący addon-źródło
- Akcje per item: Zapisz jako risks / Odpowiedz / Otwórz transkrypcję / Draft follow-upu

**Prawa kolumna (Sidebar — contributions z innych addonów):**

1. **Zespół projektu (u nas)** — z `deal_responsible_persons`:
   - Lista osób z rolami (z [katalogu ról](./00-platform-roles-catalog.md))
   - Lead chip dla `is_leader=true`
   - „auto" chip dla osób auto-dołączonych z org (np. approver)
   - **„+ Dodaj osobę z rolą"** → modal:
     - Select osoba (z contacts.persons, kind=internal)
     - Select rola (z roles.list, kind in [sales,technical,management])
     - Checkbox is_leader, share_pct
     - „Zapisz" → `crm.add_responsible_person(deal_id, person_id, role_id, ...)`

2. **Strona klienta** — z `deal_client_contacts`:
   - Lista osób z rolami (kind=external — Decydent/Influencer/Power user)
   - **„+ Dodaj kontakt klienta"** → modal:
     - Search w contacts.persons (kind=external, current_employer=client_company_id)
     - Lub „+ Nowy kontakt" → contacts.create_person
     - Select rola (kind=external)
     - „Zapisz"

3. **Zadania** — z `activity.list_tasks(linked_resources: {deal})`:
   - Checkboxy do mark complete
   - „+ Nowy task" → activity.create_task

4. **Spotkania** — z `calendar.list_events_for_resource({deal})`:
   - „+ Zaproponuj spotkanie" → ai_broker.invoke_tool('calendar.propose_meeting', {participants, deal_id})

5. **Dokumenty** — z `documents.list_for_resource({deal})`:
   - Klik → modal viewer
   - Drag-drop dla upload

6. **Koszty & refaktury** — z `billing.list_cost_allocations_for_deal(deal_id)`:
   - Plan kosztów (z `trade_costs`)
   - Marża plan
   - E-dokumenty kosztowe podpięte
   - „X do akceptacji" badge

7. **Ryzyka** — z `deals.risks` JSONB:
   - Lista chips z severity
   - „+ Dodaj ryzyko" → modal

### C4 — Tworzenie deala AI

**Workflow (kluczowy AI use case):**

1. User otwiera modal („+ Nowy deal" w pipeline / C13 palette)
2. Default tryb: **AI · jeden ruch**
3. User wpisuje natural language: „Załóż deal dla mBank na migrację CBA, 120k, koniec maja, decydent Magda Wiśniewska (CIO), commit tak, ryzyko: konkurencja Comarch"
4. Klik **„Parsuj"** wywołuje pipeline:
   ```
   a. ai_broker.invoke_tool('crm.create_deal_from_text', {raw_text, user_id})
   b. Tool:
      - LLM parsuje text → strukturalny input
      - Wywołuje contacts.find_or_create_company(name=mBank) → company_id
      - Wywołuje contacts.find_or_create_person(name=Magda Wiśniewska, company_id) → person_id
      - Składa Deal proposal: {name, value, stage(z 'budżet zaakceptowany' → offer), commit:true, est_close, owner:current_user, section: current_user.section, tags:[konkurencja:comarch]}
      - Wykrywa risks: konkurencja Comarch
      - Zwraca proposal (NIE zapisuje)
   c. UI pokazuje confirm dialog z pełnym diffem (jak w C4 mockup)
   d. User akceptuje → ai_broker.invoke_tool('crm.create_deal', {parsed_proposal}, confirmed=true)
   e. INSERT do deals + deal_client_contacts (Magda jako decydent) + deal_responsible_persons (user jako lead handlowiec)
   f. Emit event deal.created
   g. Modal się zamyka, page navigates do C3 dla nowego deala
   ```
5. Alternatywnie tryb **„Klasycznie"** — pełny formularz wszystkich pól.

### C5 / C6 / C7 — Dashboardy

**Jeden silnik widgetów (`tf-dashboard`) renderuje wszystkie 3.** Różnica = preset query + układ.

**Widgety:**

1. **Cel mc/q/y** — KPI tile z value + delta + progress bar
2. **Commit forecast** — KPI tile
3. **Pipeline ważony** — KPI tile (wartość × probability)
4. **Trend 6mc** — bar chart (`crm.compute_trend(scope, range)`)
5. **Top deale** — tabela top-5 wg wartości × probability
6. **Wymaga uwagi** — top-N signals dla użytkownika (z activity)
7. **Ranking zespołu** (tylko dyrektor/zarząd) — `crm.compute_team_ranking(section_id)`
8. **Funnel sekcji** — count + value per stage
9. **Ryzyko pieniężne** — lista deali w red zone (signals)
10. **Marża trend** — z `billing.compute_margin_for_section(section_id, range)`

**Akcje:**
- **„Edytuj widgety"** (top right) → drag-drop reorder + add/remove + change query parameters
- **Klik wartości w KPI** → drill-down (np. klik „Pipeline ważony 3.4mln" → C2 z preset filter)
- **Klik rzędu w tabeli top-deale** → C3
- **„Edytuj cel"** (na KPI Cel) → modal z setting target_pln per period

**Preset per rola** (z `roles` flagi):
- `handlowiec_*` → preset C5 (my cel, my pipeline, my signals)
- `section_director` / `flag:see_all_in_section` → preset C6 (sekcja, ranking, diagnostyka)
- `flag:see_everything` → preset C7 (org, marża, ryzyko pieniężne)

User może swój preset modyfikować — zapisywane w `dashboard_layouts(user_id, kind, layout JSONB)`.

### C8 — Forecast workbench

**Cel:** dla dyrektora i zarządu. 3 poziomy forecast: commit/best/worst + snapshoty.

**Akcje:**

1. **Toggle periodu:** Tydzień / Miesiąc (default) / Kwartał
2. **Bar chart snapshotów commit'u w czasie** — pokazuje jak commit rósł w miesiącu (z `forecast_snapshots`)
3. **„Zatrzaśnij"** (top right) → manual snapshot now (poza daily cronem) → INSERT forecast_snapshots
4. **„Snapshoty"** → drop-down z historyczny snapshotów do porównania
5. **Tabela Breakdown** — co składa się na commit (każdy deal × prawdopodobieństwo):
   - Inline edit prawd. close (mogę „obniżyć" prawdopodobieństwo)
   - Klik wiersza → C3

### C9 — Karta akceptacji budżetu

**Workflow (długi — kluczowy biznesowo):**

1. **Generowanie:** handlowiec na C3 klika „Wyślij kartę akceptacji" (action button)
   ```
   a. Walidacja: deal.stage in [offer, commit] AND deal.value_pln known AND deal.commit=true OR akceptacja_threshold matched
   b. ai_broker.invoke_tool('crm.send_acceptance_card', {deal_id, approver_person_id?}) — act, required
   c. Tool wewnątrz crm:
      i.   Wybiera approver: jeśli explicit → ten; jeśli null → z org structure: najniższe stanowisko w reports_chain (going up from owner) z flagą can_approve_budget i threshold ≥ deal.value_pln
      ii.  Wywołuje documents.render_from_template('acceptance_card_v3', field_values) → file_id
      iii. INSERT acceptance_cards (z 3 tokenami HMAC, valid_until = now + valid_days)
      iv.  UPDATE deals SET acceptance_card_file_id, acceptance_card_status='sent'
      v.   Wywołuje email.send do approvera (mail w mockupie C9) z 3 linkami
      vi.  Emit event 'acceptance_card.sent'
   d. Confirm dialog pokazuje: kto będzie approverem, jakie linki w mailu, valid_until
   ```

2. **Kliknięcie linku przez approvera** (poza systemem — z maila):
   - URL: `/links/acceptance/{deal_id}?token={hmac}&decision={accept|decline}`
   - Endpoint HTTP w core platformie (poza addonem — bo approver może nie być userem TentaFlow):
     - Walidacja token (HMAC + valid_until + nieużyty)
     - Wywołuje `crm.acceptance_card_decided(deal_id, decision, decided_by_person_id, metadata)`
     - UPDATE acceptance_cards + UPDATE deals
     - Emit event `acceptance_card.decided`
   - Po akceptacji handlowiec dostaje signal w C1 inbox „karta zatwierdzona → promuj do realizacji"

3. **W C9 mockup widać:**
   - Status workflow (timeline: generated → sent → opened → decided)
   - Preview maila wysłanego
   - Preview PDF karty
   - **„Promuj do realizacji"** button (gdy accepted) — opisany wyżej

### C10 — Cost refactor (już opisany w [Billing](./13-addon-billing.md))

W CRM addon mamy view contribution `deal.detail.sidebar` → „Koszty & refaktury" które pokazuje stan. Realna akcja przez billing addon.

### C11 — AI chat sidebar (już opisany w [04 AI Broker](./04-platform-ai-tool-broker.md))

W CRM context (gdy chat otwarty z deal detail), pula tooli jest rozszerzona o wszystkie crm.* + billing.* + email.* + activity.* + contacts.*.

### C12 — Smart inbox „Wymaga uwagi"

**Cel:** zakładka grupująca signals (z [activity](./11-addon-activity.md) rules engine + AI hygiene + AI extraction) w grupy tematyczne.

**Layout:** 4-5 grup pionowo, każda z own list signals.

**Akcje:**

1. **„Dostrój reguły"** (top right) → activity addon's rules list (z [11](./11-addon-activity.md))
2. **Filtry chips:** Wszystkie / Krytyczne / Reguły / AI / Odłożone
3. **„Edytuj regułę"** w nagłówku grupy → activity rule editor
4. Per signal — akcje analogiczne do C1 (default action z signal.default_action, dismiss, snooze)

### C13 — Command palette ⌘K (opisany w [04 AI Broker](./04-platform-ai-tool-broker.md))

Pula wyszukiwania zawiera m.in.:
- Deale (search po name/code)
- Contacts (z contacts.search)
- Akcje crm.* (create_deal, move_stage etc.)
- AI tools (z risk badge)
- Nawigacja (Pipeline / Forecast / Dashboard)

### C14 — Stages settings (administrowalny pipeline)

**Cel:** admin definiuje pipeline'y i fazy. Multiple pipelines możliwe (`pipelines` table) — domyślny + można dodać alternatywne.

**Akcje:**

1. **„Dodaj fazę"** → modal: name, slug (auto-gen), prawd., kolor, required_fields, automations
2. **Drag-drop reorder** faz w preview top
3. **Klik wiersza** → edytor fazy (jak w mockupie)
4. **Per faza:**
   - Required fields toggles (commit/acceptance_card_accepted/escalation_complete itp.)
   - Automatyzacje list (CRUD):
     - Każda automatyzacja: trigger (`on_enter_stage`, `on_exit_stage`, `on_field_change`), action (`clone_costs`, `create_task`, `emit_event`, `send_notification`)
   - „Klon plan kosztów → realizacyjne" jest preconfigured automation dla wejścia w stage=realization

5. **Po zapisie:** UPDATE pipeline_stages; pokazuje warning „Zmiany dotkną N aktywnych deali"

## Provided contracts

**Resources:** `deal`, `pipeline_stage`, `acceptance_card`, `trade_cost`, `project_income`, `forecast_snapshot`

**Queries:**
- `crm.list_deals(filter)` — z respect permissions (returns subset user może widzieć)
- `crm.get_deal(id)`
- `crm.deals_for_contact(contact_id)` — view contribution
- `crm.deals_for_company(company_id)` — view contribution
- `crm.list_deals_for_user(user_id, role: 'owner'|'team'|'mentioned')`
- `crm.compute_deal_insights(deal_id)` — AI insights cache
- `crm.compute_trend(scope, period)`
- `crm.compute_team_ranking(section_id, period)`
- `crm.forecast(scope, period, snapshot_date?)` — current or historic

**Actions:**
- `crm.create_deal(input)` (act, required)
- `crm.update_deal(deal_id, patch)` (act, required dla wartości/owner/commit)
- `crm.move_stage(deal_id, new_stage, reason?)` (act, required)
- `crm.promote_to_realization(deal_id)` (act, required — wielka transakcja)
- `crm.mark_won(deal_id, realization_date)` (act, required)
- `crm.mark_lost(deal_id, lost_reason)` (act, required)
- `crm.add_responsible_person(deal_id, person_id, role_id, is_leader?)` (act)
- `crm.remove_responsible_person(deal_id, person_id, role_id)` (act)
- `crm.add_client_contact(deal_id, person_id, role_id, is_primary?)` (act)
- `crm.send_acceptance_card(deal_id, approver_person_id?)` (act, required)
- `crm.acceptance_card_decided(deal_id, decision, decided_by_person_id, metadata)` (act — wywoływany przez link endpoint)
- `crm.add_risk(deal_id, label, severity, source?)` (act)
- `crm.add_trade_cost(deal_id, cost_input)` (act)
- `crm.add_project_income(deal_id, income_input)` (act)

**Views:**
- `contact.detail.sidebar` → „Aktywne deale" + „Historycznie wygrane"
- `company.detail.sidebar` → „Pipeline" mini
- `company.detail.main` → tabela wszystkich deali firmy
- `dashboard.handlowiec/dyrektor/zarzad` → widgety jak wyżej

**Events:**
- `crm.deal_created`
- `crm.deal_updated` (z diffem)
- `crm.deal_stage_changed` (z previous_stage)
- `crm.deal_commit_changed`
- `crm.deal_value_changed`
- `crm.deal_promoted_to_realization`
- `crm.deal_won` / `crm.deal_lost`
- `crm.responsible_person_added/removed`
- `crm.client_contact_added/removed`
- `crm.acceptance_card_sent / decided`
- `crm.trade_cost_added/updated`
- `crm.project_income_added/updated`
- `crm.risk_added`
- `crm.forecast_snapshot_taken`

**AI Tools (pełna lista ze schemami):**

| Tool name | Risk | Confirm | Description | Input schema |
|---|---|---|---|---|
| `crm.create_deal` | act | required | Tworzy deal | `{name, client_company_id, value_pln, stage?, commit?, est_close?, owner_id?}` |
| `crm.create_deal_from_text` | act | required | NL → deal proposal | `{raw_text}` |
| `crm.update_deal` | act | required | Patch dla deala | `{deal_id, patch: {...}}` |
| `crm.update_deal_from_context` | act | required | Update z transkrypcji/maila | `{deal_id, source: {kind, ref}}` |
| `crm.move_stage` | act | required | Zmienia stage | `{deal_id, new_stage, reason?}` |
| `crm.promote_to_realization` | act | required | Transakcja klonu | `{deal_id}` |
| `crm.send_acceptance_card` | act | required | Generuje + wysyła kartę | `{deal_id, approver_person_id?}` |
| `crm.read_deal` | read | none | Detal deala | `{deal_id}` |
| `crm.search_deals` | read | none | Wyszukiwanie | `{query, filters?}` |
| `crm.forecast_deal` | draft | none | Prawd. close z reguł | `{deal_id}` |
| `crm.compute_deal_insights` | read | none | 3 obserwacje AI | `{deal_id}` |

## Consumed contracts

```toml
[needs.platform]
permissions = ["can", "list_for_user"]
roles = ["read"]
org = ["read", "get_subordinates", "is_subordinate_of"]
ai_broker = ["register_tool", "request_call"]

[needs.contacts]
search, get_person, get_company, list_persons_in_company,
find_or_create_person, find_or_create_company  # przez AI broker

[needs.activity]
create_task, create_signal, read_timeline, publish_event

[needs.documents]
render_from_template, upload, get_file, list_for_resource

[needs.billing]
list_cost_allocations_for_deal, compute_margin_for_deal, compute_margin_for_section

[needs.calendar]
list_events_for_resource, propose_meeting

[needs.email]
list_threads_for_resource, draft_followup, send
```

## Permissions (visibility)

Z [02 permissions](./02-platform-permissions.md):

| Reguła | Subject | Action | Condition |
|---|---|---|---|
| Handlowiec widzi swoje | `flag:can_edit_deal` | read+write | `owner_is_self OR assigned_to_self` |
| Manager widzi sekcję | `flag:see_all_in_section` | read | `section_of_self` |
| Director widzi dział | `flag:see_all_in_dept` | read | `dept_of_self` (transitive via org) |
| CEO widzi wszystko | `flag:see_everything` | * | `{}` |
| Approver czyta dla akceptacji | `flag:can_approve_budget` | read+approve | `value_pln <= self.threshold` |
| Technical osoba na projekcie | `kind=technical AND assigned_to_self` | read+write | `assigned_to_self` |

## Migracja z IntrApp

Pełne mapowanie pól w [LIFECYCLE.md](../LIFECYCLE.md). Najważniejsze:
- `ProjectAttributes` → `deals` (z polami jak w LIFECYCLE)
- `StatusListStatus=100` → `stage='realization'`
- `OfferChanceOnly` → odrzucone (martwe pole)
- `IsTradeProject` → odrzucone (wszystkie deale w nowym CRM są handlowe)
- `TradeResponsiblePersons` → `deal_responsible_persons` (z mapowaniem `JobId` → `role_id` przez katalog ról)
- `ProjectContactPersons` → `deal_client_contacts`
- `Sections.ManagerId` → org structure + flagi roli
- `Approver/AccountHolder` IDs → `deal_responsible_persons` z odpowiednią rolą (z katalogu)
- `TradeCosts` → `trade_costs` (po promocji do realization: klon do `trade_realization_costs`)
- `ProjectIncomes` → `project_incomes`
- `DocumentCostProjects` → `billing.cost_allocations` (osobny addon)

Pomijamy:
- Pola identyfikowane jako martwe (LIFECYCLE.md)
- 3 sztywne subprojekty Presales/Realizacja/Utrzymanie (zastępujemy fazami)
- `ProductNotes` (TentaFlow ma `activity.timeline_events`)

## Implementation order

### Faza A — MVP testu adopcji (najmniejszy zestaw)

1. Schema deals + deal_responsible_persons + deal_client_contacts + pipeline_stages.
2. Seed pipeline default (7 stage'ów).
3. Host fn CRUD (`list_deals`, `get_deal`, `create_deal`, `update_deal`, `move_stage`).
4. UI C3 Deal detail — read-only (z mock data).
5. UI C3 — inline edit + responsible_persons add/remove.
6. UI C2 Pipeline kanban (read-only z deals data).
7. UI C2 — drag-drop move_stage.
8. UI C4 Deal create — klasycznie (formularz).
9. **Test adopcji:** czy 2-3 handlowców używa codziennie?

### Faza B — Daily Inbox + AI

10. C1 Daily Sales Inbox — agregat z `activity.list_signals` + pewne crm-specific signals.
11. C13 Command palette — search po dealach + akcje.
12. AI tools: `crm.create_deal_from_text`, `crm.update_deal_from_context`.
13. C11 AI chat sidebar.
14. C4 — tryb AI (parse text → confirm → create).

### Faza C — Workflow akceptacji

15. Schema acceptance_cards.
16. UI C9 (status workflow + buttons).
17. PDF rendering (przez documents addon).
18. Email link endpoint (poza addonem — w core HTTP).
19. UI „Promuj do realizacji" z transakcją klonu.

### Faza D — Pipeline finansowy

20. Schema trade_costs + project_incomes.
21. UI w deal detail (sekcje plan kosztów + plan przychodów).
22. Integration z billing (view contributions sidebar „Koszty & refaktury").
23. UI C10 (cost refactor) — w billing addonie, ale CRM dostarcza deal context.

### Faza E — Dashboards

24. Schema `dashboard_layouts`.
25. Engine widgetów (`tf-dashboard`) — uniwersalny.
26. UI C5/C6/C7 — różne presets per rola.
27. AI insights bar (compute_deal_insights cache).

### Faza F — Forecast

28. Schema forecast_snapshots.
29. Daily cron snapshot.
30. UI C8 forecast workbench.

### Faza G — Smart inbox + reguły

31. Custom CRM-specific signals (np. `commit_value_dropped_by_20pct`).
32. UI C12 grupowanie signals.
33. Rules editor w platform/activity.

### Faza H — Settings + admin

34. UI C14 stages settings — administrowalny pipeline.
35. UI dla automations (per stage).
36. Migracja z IntrApp.

## Otwarte decyzje

1. **Probability % — manual czy auto?** Mocna pokusa: derive z reguł (faza × wiek deala × aktywność). Rekomendacja: **default z faza.probability_pct, override per deal, AI sugeruje update**.

2. **Multi-pipeline** (różne pipelinely dla różnych typów dealów) — czy w MVP? Rekomendacja: **schema gotowa (`pipeline_id` na dealu), MVP jeden pipeline, multi w v2 gdy będzie zapotrzebowanie**.

3. **Subprojekty/etapy** w realizacji — czy potrzebne? IntrApp miał sztywne, my pominęliśmy. Rekomendacja: **na MVP brak; gdy stage=realization, dodajemy „milestones" w deal jako simple checklist; pełny moduł WBS jeśli klient zapyta**.

4. **Commissions / prowizje** — czy ten CRM ma liczyć prowizje handlowców? Field `share_pct` w responsible_persons jest. Rekomendacja: **infrastruktura jest (`share_pct`), ale realny calculator prowizji to osobny addon `commissions` v2**.

5. **Multi-currency forecast** — jak agregować PLN/EUR/USD? Rekomendacja: **konwersja do PLN w snapshot, z `exchange_rate` snapshot dla audit**.

6. **„Realization status" granularność** — text dziś, structured w v2 (np. milestones)? Rekomendacja: **text MVP, structured jak będzie WBS module**.

7. **Lost reason taxonomy** — free-text czy enum? Rekomendacja: **enum z catalogiem `lost_reasons` (Cena/Konkurencja/Zmiana priorytetów/Budżet/Inne) + free-text dla „Inne"** — żeby było analyzowalne.
