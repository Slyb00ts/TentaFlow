# 14 · Addon — Calendar

**Mockup:** [L1 Kalendarz tydzień](../../../../../.gstack/projects/Slyb00ts-TentaFlow/designs/crm-v1/l01-calendar.html)

## Cel

**Connector** do zewnętrznego kalendarza (Outlook/Microsoft 365, Google Calendar, iCloud) + warstwa kontekstu biznesowego (spotkanie ↔ deal/contact/firma). NIE budujemy własnego klienta kalendarza.

Wartość dodana TentaFlow:
1. **Meeting brief AI** — przed spotkaniem wyświetla streszczenie historii klienta + ostatnie ustalenia + sugerowaną agendę
2. **Auto-link** wydarzeń do dealów na podstawie uczestników
3. **Meeting assistant** — gdy mamy transkrypcję (z osobnego serwisu), wyciąga obiekcje/wymagania/next steps i proponuje update deala
4. **Calendar view** wpięty w deal/contact detail jako contribution

## Domeny i encje

### `calendar_accounts` (sub-kont per user)

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `user_id` | UUID FK | |
| `provider` | ENUM | `outlook | google | icloud | manual` |
| `account_email` | TEXT | |
| `oauth_token_encrypted` | BYTES | |
| `refresh_token_encrypted` | BYTES | |
| `last_sync_at` | TIMESTAMP | |
| `sync_status` | ENUM | `active | error | paused` |

### `events` (mirror zewnętrznych eventów + nasze meta)

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `external_id` | TEXT | ID w Outlook/Google |
| `calendar_account_id` | UUID FK | |
| `title` | TEXT | |
| `description` | TEXT | |
| `location` | TEXT | „Sala konf. Z3" / „Google Meet link" |
| `start_at`, `end_at` | TIMESTAMP | |
| `is_all_day` | BOOL | |
| `organizer_email` | TEXT | |
| `linked_resources` | JSONB | Lista `{type, id}` — autodetektowane lub manualne |
| `meeting_kind` | ENUM | `internal | client_demo | client_followup | sales_call | other` (auto-classify) |
| `has_transcript` | BOOL | Czy z meeting bot |
| `transcript_file_id` | UUID FK → documents.files | |
| `ai_summary` | TEXT | Streszczenie AI po spotkaniu |
| `ai_extracted_actions` | JSONB | Obiekcje / next steps / decyzje |
| `external_etag` | TEXT | Do sync conflict resolution |
| `updated_at` | TIMESTAMP | |

### `event_attendees`

| Pole | Typ | Opis |
|---|---|---|
| `event_id` | UUID FK | |
| `email` | TEXT | |
| `name` | TEXT | |
| `linked_person_id` | UUID FK → contacts.persons | (gdy mamy match) |
| `is_internal` | BOOL | Computed: linked_person.kind=internal |
| `response_status` | ENUM | `accepted | tentative | declined | needs_action` |

### `meeting_briefs` (przygotowane briefy AI)

| Pole | Typ | Opis |
|---|---|---|
| `id` | UUID PK | |
| `event_id` | UUID FK | |
| `generated_at` | TIMESTAMP | |
| `participants_context` | JSONB | Historia kontaktów z każdym uczestnikiem |
| `deal_context` | JSONB | Snapshot related deal |
| `previous_meetings` | JSONB | Ostatnie 3 spotkania z tymi osobami |
| `suggested_agenda` | TEXT | |
| `objections_to_address` | TEXT[] | Z historii (Comarch wzmiankowany, cena za wysoka itd.) |
| `recommended_materials` | UUID[] | file_ids z documents |

## Lifecycle eventu

```
External calendar (Outlook/Google) creates event
  → Sync job ingests → events table populated
  → AI auto-classify meeting_kind (na bazie tytułu + uczestników)
  → AI auto-link to deal (jeśli uczestnik to known contact z aktywnym dealem)
  → Pre-meeting (T-30min): generate meeting_brief → notify owner
  → Meeting in progress (if meeting bot active): transcript captured
  → Post-meeting (T+5min after end): AI summary generated → signal w activity
  → User reviews summary, accepts AI extracted actions (update deal etc.)
```

## UI surfaces

### L1 — Calendar tydzień

**Layout:** sidebar (foldery/kalendarze toggle) + grid tygodniowy.

**Akcje:**

1. **Nawigacja:** ←/Dziś/→ (top right)
2. **Toggle widoku:** Dziś / Tydzień / Miesiąc
3. **Toggle kalendarzy** w sidebar: mój / zespół / sale konf — wybór skąd ładować
4. **Klik wydarzenia** → modal detail (uczestnicy, brief AI, linked deal, akcje)
5. **„+" (top right)** → nowy event (lokalny — sync do zewnętrznego kalendarza)
6. **Drag-drop wydarzenia** → reschedule (push do zewnętrznego)
7. **Pasek z briefem AI** dla najbliższego eventu pod kalendarzem — szybki preview

### Modal detail eventu

- Uczestnicy z avatarami (linked / nieznani)
- Kontekst biznesowy: linked deal/contact/company (z możliwością unlink + manual link)
- Brief AI z sekcjami (z poprzednich kontaktów / obiekcje / suggested agenda / materiały)
- Po spotkaniu: transcript link + AI summary + extracted actions z buttonami „Zaakceptuj update deala"

### Wewnątrz innych addonów (contributions)

**deal.detail.sidebar** → „Spotkania" sekcja (już używana w C3):
- Lista past + future meetings dla tego deala (z `events` filtrowanych po `linked_resources`)
- „+ Zaproponuj spotkanie" → woła `calendar.propose_meeting` tool

**contact.detail.sidebar** → analogicznie dla osoby (z `event_attendees.linked_person_id`).

**dashboard.handlowiec** → widget „Najbliższe spotkania".

## Provided contracts

**Resources:** `event`, `meeting_brief`

**Queries:**
- `calendar.list_events(filter: {user_id?, start_after?, start_before?, linked_resource?})`
- `calendar.get_event(id)`
- `calendar.list_events_for_resource(linked_resource)` — view contribution helper
- `calendar.find_free_slots(participants[], duration_min, range)` — dla AI tool propose_meeting

**Actions:**
- `calendar.connect_account(provider, oauth_code)` (act)
- `calendar.sync_now(user_id)` (act)
- `calendar.create_event(input)` (act, required — push do zewnętrznego)
- `calendar.update_event(id, input)` (act, required)
- `calendar.cancel_event(id)` (act, required)
- `calendar.link_to_resource(event_id, linked_resource)` (act — ręczny link)
- `calendar.accept_ai_extracted_actions(event_id, action_ids[])` (act, required)

**Views:**
- `deal.detail.sidebar` → „Spotkania"
- `contact.detail.sidebar` → „Spotkania"
- `dashboard.handlowiec` → „Najbliższe spotkania"

**Events:**
- `calendar.event_created` (z zewnątrz lub naszego UI)
- `calendar.event_updated`
- `calendar.event_canceled`
- `calendar.meeting_started` (gdy `now >= start_at`)
- `calendar.meeting_ended` (post-meeting trigger)
- `calendar.transcript_available`
- `calendar.ai_summary_ready`
- `calendar.event_linked_to_deal`

**AI Tools:**
- `calendar.propose_meeting(participants[], duration_min, preferred_range)` (draft, 1-click) — 3 sloty
- `calendar.generate_meeting_brief(event_id)` (read) — generuje brief
- `calendar.summarize_meeting(event_id)` (read) — używa transcript jeśli jest
- `calendar.extract_actions_from_transcript(transcript_file_id)` (read) — wyciąga obiekcje / next steps

## Consumed contracts

```toml
[needs.platform]
permissions, ai_broker
[needs.contacts]
search = []  # do match uczestnika po email
get_person = []
[needs.crm]
read_deal = []
list_deals_for_contact = []  # do auto-link
update_deal_from_context = []  # po AI extract z meetinga
[needs.documents]
get_file = []  # transcript
[needs.activity]
publish_event = []
create_signal = []  # post-meeting summary jako signal w inbox
```

## Permissions

- Mój kalendarz — widoczny tylko dla mnie
- Kalendarz zespołu — dla osób na tym samym poziomie/sekcji org structure
- Linked events do deala — visibility z deala
- Transcript — tylko uczestnicy meetingu + manager (z org)

## Provider integrations (sync technique)

| Provider | Protocol | Sync method |
|---|---|---|
| Outlook / M365 | Microsoft Graph API | Webhook subscriptions + polling fallback |
| Google Calendar | Google Calendar API | Push notifications + sync token |
| iCloud | CalDAV | Polling (CalDAV nie ma push) |
| Manual | — | UI only (events tworzone w TentaFlow, brak sync) |

OAuth flow: user łączy konto raz, tokens trzymane szyfrowane. Sync co 5 min (incremental).

## Migracja z IntrApp

IntrApp nie miał kalendarza zintegrowanego. Wszystko nowe. Migracja:
- `Tasks` z polem typu „spotkanie" (jeśli używane) → import jako manual events w TentaFlow calendar.
- Brak rzeczywistych eventów z Outlook → user musi podłączyć kalendarz po starcie.

## Implementation order

1. Schema: calendar_accounts, events, event_attendees, meeting_briefs.
2. OAuth flow dla Outlook (priorytet — najpopularniejszy enterprise).
3. Sync worker (incremental, z conflict resolution po external_etag).
4. UI L1 read-only — wyświetlanie eventów z bazy.
5. Auto-link AI: po sync match uczestnika po email → linked_person_id, potem `linked_resources` na bazie aktywnych deali.
6. Host fn read.
7. UI L1 — create/update event z push do zewnętrznego.
8. Modal detail event.
9. View contributions sidebar dla CRM/Contacts.
10. AI tool propose_meeting (free slots finder).
11. Meeting brief generator (T-30min cron + on-demand).
12. Post-meeting workflow: AI summary + extract actions.
13. Google Calendar integration.
14. CalDAV (iCloud) — opcj.
15. Meeting transcript ingestion (z osobnego serwisu meeting_bot — pomijamy szczegóły, ale integracja przez upload do documents + event_id link).

## Otwarte decyzje

1. **Dwukierunkowy sync** — edycja w TentaFlow pushuje do Outlook (i odwrotnie)? Rekomendacja: **tak, ale z conflict resolution** (jeśli ten sam event zmieniony równolegle w obu, nowszy timestamp wygrywa, drugi staje się audit log entry).

2. **Cykliczne wydarzenia (RRULE)** — full support czy uproszczenie? Rekomendacja: **read-only support na MVP** (wyświetlamy poprawnie), edycja recurringów w external client.

3. **Privacy events** — jeśli user oznaczy event jako private w Outlook, czy widać go w TentaFlow? Rekomendacja: **respect privacy flag, pokazujemy tylko „Busy" placeholder**.

4. **Sale konferencyjne** — czy traktujemy je jako oddzielne kalendarze (zasoby do rezerwacji)? Rekomendacja: **integracja z Outlook rooms na bazie standardowych M365 features, nie własna implementacja**.

5. **Meeting bot integration** — kto generuje transcript? Rekomendacja: **osobny addon `meeting-bot`** (już istnieje w designs jako koncept) który nagrywa, transkrybuje, i publikuje przez event. Calendar tylko consumer.

6. **Email invitations** — czy TentaFlow wysyła zaproszenia (ICS) czy zostawiamy external clientowi? Rekomendacja: **delegujemy do external** — tworzymy event w Outlook, on rozsyła ICS.
