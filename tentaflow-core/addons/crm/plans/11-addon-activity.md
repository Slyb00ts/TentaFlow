# 11 · Addon — Activity

**Mockup:** [A1 timeline](../../../../../.gstack/projects/Slyb00ts-TentaFlow/designs/crm-v1/a01-activity-timeline.html)

## Cel

Wspólna warstwa **zdarzeń, zadań i przypomnień**. Tasks (todo), reminders, timeline events (logi aktywności), notatki — wszystko jednym strumieniem. Inne addony publikują eventy przez `EventPublisher`, consume'ują przez `read_timeline`. CRM Daily Sales Inbox (C1) i wszystkie dashboardy karmią się z Activity.

Bez Activity każdy addon trzyma własną osobną listę „co się działo" i niemożliwe jest spójne pokazanie historii dla kontaktu / deala / firmy.

## Domeny i encje

### `tasks`

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `title` | TEXT | „Zadzwoń do Magdy ws. obiekcji" |
| `description` | TEXT | Opcj. detal |
| `assignee_user_id` | UUID FK | Komu przypisane |
| `created_by` | UUID FK | |
| `due_at` | TIMESTAMP | Termin |
| `completed_at` | TIMESTAMP | Null = otwarte |
| `priority` | ENUM | `low | normal | high | critical` |
| `linked_resources` | JSONB | Lista `{type, id}` — do których zasobów task się odnosi (deal, contact, company) |
| `auto_generated` | BOOL | Czy stworzony przez regułę/AI |
| `source` | TEXT | `manual` / `rule:stale_commit` / `ai:hygiene` / `addon:crm` |
| `created_at`, `updated_at` | | |

### `reminders`

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `user_id` | UUID FK | Komu się przypomni |
| `remind_at` | TIMESTAMP | |
| `title` | TEXT | |
| `linked_resource` | JSONB | `{type, id}` |
| `triggered_at` | TIMESTAMP | Kiedy się odpaliło |
| `dismissed_at` | TIMESTAMP | |
| `snoozed_until` | TIMESTAMP | |
| `source` | TEXT | |

### `timeline_events` (audit log per resource)

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `event_name` | TEXT | Np. `deal.stage_changed`, `email.sent`, `meeting.held`, `task.completed` |
| `actor_user_id` | UUID FK | Kto wykonał (nullable jeśli system) |
| `linked_resources` | JSONB | Lista zasobów których dotyczy event |
| `summary` | TEXT | Krótki opis dla UI |
| `payload` | JSONB | Pełne dane eventu (z opcj. diffem) |
| `addon_instance` | TEXT | Skąd event (np. `crm/main`) |
| `created_at` | TIMESTAMP | |

Indeksy: na `linked_resources` (GIN) — szybka filtracja eventów po deal/contact/company.

### `notes`

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `body` | TEXT | Markdown (lub rich) |
| `author_user_id` | UUID FK | |
| `linked_resources` | JSONB | |
| `is_pinned` | BOOL | Pin na zasobie |
| `visibility` | ENUM | `private | team | section | all` (przy private widzi tylko autor) |
| `created_at`, `updated_at` | | |

### `signals` (sygnały AI hygiene / reguł — karmią Daily Inbox)

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `kind` | ENUM | `stale_commit | offer_no_reply | margin_drop | meeting_aftermath | cost_pending_refacture | extract_proposal | …` |
| `severity` | ENUM | `info | warning | critical` |
| `for_user_id` | UUID FK | Komu pokazać |
| `linked_resources` | JSONB | |
| `title` | TEXT | |
| `body` | TEXT | |
| `default_action` | JSONB | `{tool: "crm.update_deal", input: {...}}` — sugerowana akcja |
| `dismissed_at`, `snoozed_until` | | |
| `source` | TEXT | `rule:stale_commit_9d` / `ai:hygiene_check` / `ai:extraction` |
| `confidence` | DECIMAL | 0.0-1.0 (tylko dla AI) |
| `created_at` | TIMESTAMP | |

Signals są **różne od tasks** — task wymaga zrobienia, signal proponuje akcję (user może odrzucić). Sygnały pojawiają się w Daily Sales Inbox (C1) z badge'ami.

## Lifecycle

**Task:**
```
created (manual / auto-rule / AI-extract / addon-event)
  → assigned (assignee_user_id)
  → snoozed | reassigned | edited
  → completed_at = now
  → archived (po 90 dniach od completion — soft, dla audit)
```

**Reminder:**
```
created
  → remind_at reached → triggered (push notification + appears in inbox)
  → dismissed | snoozed (przesuwa remind_at)
```

**Signal:**
```
created (rule trigger / AI batch)
  → shown in inbox
  → user actions: accept default_action | edit_then_act | dismiss | snooze
  → resolved (if linked resource changes in expected way — auto)
```

## UI surfaces

### A1 — Activity timeline (główny widok addona)

**Layout:** sidebar lewa (filtr po addonie + Today/Week/Month), main timeline, prawy sidebar (tasks + reminders + signals).

**Akcje:**

1. **Filtr "Filtruj po addonie"** w sidebar — toggle widoczności eventów per addon (CRM/Email/Calendar/Billing)
2. **Toggle Today/Week/Month** w topbar — okno czasowe
3. **Klik wiersza eventu** → drill do zasobu (np. klik „dodano kontakt Patrycja Mazur" → K2 osoby)
4. **„+" przy taskach** → modal nowego taska (form: title, due, linked deal, priority)
5. **Checkbox przy tasku** → completed_at = now, dispatch event `task.completed` (publikuje też się w timeline)
6. **Klik sygnału AI** → otwiera default_action z confirm dialog (z [04 broker](./04-platform-ai-tool-broker.md))
7. **„Pomiń sygnał"** → dismissed_at = now
8. **„Odłóż"** → snoozed_until = wybrana data (jutro, za tydzień, custom)

### Wewnątrz innych addonów (contributions)

Activity wystawia view contributions:
- `contact.detail.main` → unified timeline dla osoby (agregat eventów z linked_resources containing contact_id)
- `company.detail.main` → unified timeline dla firmy
- `deal.detail.main` → unified timeline dla deala (już używany w C3)
- `dashboard.handlowiec` → sekcja „Moja aktywność tygodnia" + sekcja „Moje zadania"

## Provided contracts

**Resources:**
- `task`
- `reminder`
- `timeline_event`
- `note`
- `signal`

**Queries:**
- `activity.list_tasks(filter: {assignee?, due_before?, completed?, linked_resource?})`
- `activity.read_timeline(linked_resource: {type, id}, limit?, types?)` — kluczowa funkcja: zwraca chronological events dla danego zasobu
- `activity.list_signals(user_id, filter: {kind?, severity?, dismissed?})`
- `activity.get_user_workload(user_id, range)` — liczba taskow / spotkań / aktywności w okresie

**Actions:**
- `activity.create_task(input) → Task`
- `activity.complete_task(task_id) → Task`
- `activity.create_reminder(input) → Reminder`
- `activity.create_note(input) → Note`
- `activity.dismiss_signal(signal_id, reason?)`
- `activity.snooze_signal(signal_id, until)`

**Views:**
- contribution dla `contact.detail.main`, `company.detail.main`, `deal.detail.main`, `dashboard.*`

**Events:**
- `activity.task_created / completed / overdue`
- `activity.reminder_triggered`
- `activity.signal_created / dismissed`
- `activity.note_added`

**AI Tools:**
- `activity.create_task` (act, required confirm) — głosowo „dodaj task zadzwoń do magdy jutro" → confirm → save
- `activity.suggest_next_actions(user_id, context)` — read, AI proponuje 3 najlepsze następne akcje
- `activity.summarize_activity_for_resource(linked_resource)` — read, AI streszcza co się działo

## Consumed contracts

```toml
[needs.platform]
permissions = ["can"]
roles = ["read"]

[needs.contacts]
get_person = []
search = []
```

Activity nie wymaga dużo — ona głównie ZBIERA eventy z innych addonów (przez event bus, nie pull) i pokazuje.

## Permissions

- Task widoczny dla assignee + jego managera (z org structure)
- Note z `visibility=private` — tylko autor
- Note z `visibility=team` — autor + jego team (z org)
- Signal — tylko `for_user_id`
- Timeline events — zgodnie z permissions na linked_resource (jeśli user nie widzi deala, nie widzi jego eventów)

## Reguły generujące signals (silnik reguł)

Activity ma wbudowany **rules engine** który okresowo skanuje system i emituje signals.

Standardowe reguły (preinstalowane):

| Reguła | Kiedy | Severity | Default action |
|---|---|---|---|
| `stale_commit_7d` | deal.commit=true AND deal.last_activity_at < now()-7d | critical | sugeruje zadzwonić lub zdjąć commit |
| `offer_no_reply_14d` | deal.stage=offer AND ostatnia outgoing mail >14d temu AND brak otwarcia | warning | nudge AI mailem |
| `margin_drop` | deal.margin_pct spadło >2pp w ostatnich 7d | warning | open deal + review |
| `meeting_no_followup` | spotkanie zakończone >24h temu AND brak nowego eventu na dealu | info | „utwórz follow-up task" |
| `cost_pending_refacture` | document_cost.is_accepted=true AND brak attach_cost_to_deal w 48h | warning | „refakturuj na deal X" |
| `empty_pipeline` | user IS handlowiec AND własny pipeline_value < threshold | warning | dla menedżera (nie samego usera!) |

Reguły są **administrowalne** — admin może wyłączyć / dodać własne w UI (lista reguł podobna do `permission_rules` z [02](./02-platform-permissions.md)). Custom rules używają tego samego predicate language.

## Migracja z IntrApp

IntrApp miał `Tasks`, `TaskAttributes`, `Reminders`, `ProductNotes` jako audit log per właściciel.

Mapowanie:
- `TaskAttributes` → `tasks` (z `linked_resources` zawierającym `{type:"deal", id: ProjectId}`)
- `ProductNotes` → `timeline_events` jako historyczne wpisy
- `Reminders` → `reminders`
- Free-form notes → `notes` jeśli były wpisywane przez handlowców

Pomijamy: `ProductNotes` dla rzeczy które TentaFlow rejestruje przez własny audit log (już mamy).

## Implementation order

1. Schema: tasks, reminders, timeline_events, notes, signals.
2. Host fn read: list_tasks, read_timeline (kluczowa!), list_signals.
3. UI A1 timeline read-only.
4. Host fn write: create_task, complete_task, create_reminder, create_note.
5. UI A1 — task creator + complete checkbox + signals dismiss/snooze.
6. Event bus subscription — register handlery które pakują eventy z innych addonów do timeline_events.
7. View contributions — sidebar timeline w innych addonach.
8. Reminders worker — codzienne crony skanujące remind_at i wysyłające push notifications + tworzenie signals.
9. Rules engine — daily/hourly scan rules, generate signals.
10. UI Rules admin (lista reguł, toggle, custom).
11. AI tools (create_task, suggest_next_actions, summarize).
12. Migracja z IntrApp.

## Otwarte decyzje

1. **Czy notes powinny mieć rich text (markdown)?** Tak, ale renderer prosty (bold/italic/lists/links). Bez tabel/embed. Rekomendacja: **Markdown subset + GFM checkboxes**.

2. **Snooze granularity** — godziny / dni / „następny tydzień roboczy"? Rekomendacja: **preset chips (1h, jutro, za tydzień, custom)** + custom date picker.

3. **Konflikty signals** — jeśli 3 reguły wystawią sygnał dla tego samego deala, czy merge'ujemy? Rekomendacja: **dedup po `(kind, linked_resource)` z 24h window** — nowszy nadpisuje starszy.

4. **Reminders push channel** — desktop notification, mail, oba? Rekomendacja: **user wybiera per kind sygnału w preferences** (`notification_preferences` tabela).

5. **Activity vs Audit log TentaFlow** — TentaFlow już ma własny `audit_log`. Activity timeline_events wygląda podobnie. Rekomendacja: **audit_log = systemowy (security, RODO), timeline_events = biznesowy (user-facing historia)**. Różne tabele, różne retention policies.

6. **Auto-tasks po stage change** — czy gdy deal przejdzie do Realizacji automatycznie stworzyć task „dodaj PM"? Rekomendacja: **tak, ale jako signal nie hardcoded task** — daje userowi kontrolę.
