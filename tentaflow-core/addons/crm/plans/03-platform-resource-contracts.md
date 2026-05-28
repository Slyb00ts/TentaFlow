# 03 · Platform — Resource contracts (kontrakty między addonami)

## Cel

Wzorzec **Resource Provider + View Contributions + Action Provider** dla komunikacji między addonami. Każdy addon **deklaruje** w manifeście co ma (resources, queries, actions, views, events, ai_tools) i czego potrzebuje (`needs`). Host trzyma rejestr i pośredniczy — addony nie widzą się bezpośrednio.

Bez tego mechanizmu każda relacja deal↔contact↔invoice musiałaby być hardcoded w kodzie każdego addona. Z nim — nowy addon dodany jutro automatycznie wpina się w widoki kontaktu/deala bez zmian w istniejących.

Wzorce inspiracji: Salesforce AppExchange (resource model, ciężki), Notion blocks (kompozycja UI, ale problem z query), VS Code extensions (`contributes` w manifeście). Nasz model jest **lekki** — minimalny zestaw kontraktów do realnych potrzeb sprzedażowych.

## Domeny i kontrakty

### Manifest addonu (`manifest.toml`)

Każdy addon WASM ma manifest. Sekcje istotne dla resource contracts:

```toml
[addon]
slug = "crm"
version = "0.1.0"
instance = "main"  # możliwa wielokrotność: crm/main, crm/test

[provides]
resources = ["deal", "pipeline_stage"]
events = [
  "deal.created",
  "deal.stage_changed",
  "deal.commit_changed",
  "deal.promoted_to_realization",
  "deal.won",
  "deal.lost",
  "acceptance_card.sent",
  "acceptance_card.decided"
]

[[provides.queries]]
name = "crm.deals_for_contact"
input  = { contact_id = "uuid" }
output = "list<DealSummary>"
description = "Lista dealów na których kontakt jest powiązany (jako klient lub osoba)"

[[provides.queries]]
name = "crm.deals_for_company"
input  = { company_id = "uuid" }
output = "list<DealSummary>"

[[provides.actions]]
name = "crm.create_deal"
input_schema = "schemas/create_deal.json"
output_schema = "schemas/deal.json"
risk = "mutating"
required_grants = ["crm.deal.write", "contacts.read_basic"]
confirmation = "required"

[[provides.actions]]
name = "crm.move_stage"
input_schema = "schemas/move_stage.json"
risk = "mutating"
required_grants = ["crm.deal.write"]
confirmation = "required"

# UI contributions — addon zgłasza się że umie się wyrenderować w danym slocie
[[provides.views]]
slot = "contact.detail.sidebar"
panel_id = "crm.deals_for_contact"
title = "Deale"
query = "crm.deals_for_contact"
render = "list"
fields = ["name", "stage", "value_pln", "owner_name", "last_activity_at"]
empty_state = "Brak dealów dla tego kontaktu"
required_grants = ["contacts.read_basic"]

[[provides.views]]
slot = "company.detail.sidebar"
panel_id = "crm.pipeline_for_company"
title = "Pipeline"
query = "crm.deals_for_company"
render = "pipeline_mini"

[[provides.views]]
slot = "company.detail.main"
panel_id = "crm.deals_table_for_company"
title = "Wszystkie deale firmy"
query = "crm.deals_for_company"
render = "table"
order = 50  # priorytet renderowania (im niżej tym wcześniej)

# Materialized summaries — co host trzyma w cache dla instant rendera
[[provides.summaries]]
name = "crm.contact_deal_stats"
key_type = "contact_id"
fields = ["active_count", "lost_count", "won_count", "total_value_pln", "last_activity_at"]
refresh_on = ["deal.created", "deal.stage_changed", "deal.won", "deal.lost"]

[provides.ai_tools]
# patrz [04 AI Broker]

[needs]
# co my potrzebujemy od innych
contacts = ["read_basic", "read_relations", "search"]
documents = ["read_metadata", "attach_to_resource", "create_from_template"]
billing = ["read_costs_for_project", "subscribe_invoice_events"]
activity = ["create_task", "create_reminder", "publish_event", "read_timeline"]
calendar = ["read_events_for_resource", "propose_meeting"]
email = ["read_thread_for_resource", "draft_send"]

[needs.platform]
permissions = ["can", "list_for_user"]
roles = ["read"]
org = ["read"]
ai_broker = ["register_tool", "request_call"]
```

### Resource Provider pattern

Każdy addon deklaruje swoje resources. Resource ma:
- `type` (np. `deal`)
- `id` (UUID)
- `display_name` (np. „VW Poznań · ERP fleet")
- `url` (canonical link do otwarcia)
- `owner_addon_instance` (np. `crm/main`)

Inne addony nie wnikają w strukturę — operują na opaque `(type, id)` parach przez host fn.

### View Contributions — sloty

**Standardowe sloty** (zdefiniowane w core):

| Slot | Gdzie się pojawia | Co dostaje |
|---|---|---|
| `contact.detail.sidebar` | K2 Person detail sidebar | `{contact_id}` |
| `contact.detail.main` | K2 Person detail main | `{contact_id}` |
| `company.detail.sidebar` | K3 Company detail sidebar | `{company_id}` |
| `company.detail.main` | K3 Company detail main | `{company_id}` |
| `deal.detail.sidebar` | C3 Deal detail right column | `{deal_id}` |
| `deal.detail.main` | C3 Deal detail middle/top | `{deal_id}` |
| `dashboard.handlowiec` | C5 widgety | `{user_id}` |
| `dashboard.dyrektor` | C6 widgety | `{user_id, section_id}` |
| `dashboard.zarzad` | C7 widgety | `{user_id}` |
| `command_palette` | C13 results | `{query, context}` |
| `global.search` | wyniki ⌘K | `{query}` |

Każdy addon może wpiąć się w dowolny slot przez deklarację w manifeście. Shell pyta registry „kto ma contribution dla `contact.detail.sidebar` i id=X" → dostaje listę paneli → renderuje je po `order`.

**View model (co addon zwraca):**

```json
{
  "panel_id": "crm.deals_for_contact",
  "title": "Deale",
  "icon": "i-deal",
  "contrib_tag": "crm",
  "badge_count": 3,
  "render": "list",
  "empty_state": "Brak dealów dla tego kontaktu",
  "items": [
    {
      "id": "deal-uuid",
      "title": "mBank · CBA migracja",
      "subtitle": "Commit · 120k PLN · 9 dni cisza",
      "badge": {"label": "Commit", "color": "accent"},
      "url": "/crm/deals/uuid",
      "actions": [
        {"name": "open", "label": "Otwórz", "primary": true},
        {"name": "snooze", "label": "Odłóż"}
      ]
    }
  ],
  "footer_actions": [
    {"name": "add_deal", "label": "+ Dodaj deal", "tool": "crm.create_deal", "preset_input": {"contact_id": "..."}}
  ]
}
```

Shell renderuje przez komponenty `tf-*` (lista, tabela, kanban-mini itd. wg `render`).

### Materialized Summaries

Dla instant renderu sidebarów host trzyma small cache. Addon deklaruje:

```toml
[[provides.summaries]]
name = "crm.contact_deal_stats"
key_type = "contact_id"
fields = ["active_count", "lost_count", "won_count", "total_value_pln", "last_activity_at"]
refresh_on = ["deal.created", "deal.stage_changed", "deal.won", "deal.lost"]
```

Host:
1. Subskrybuje events `crm.deal.*`
2. Kiedy event przychodzi — woła `crm.compute_summary(key=contact_id)` w addonie
3. Wynik trzyma w tabeli `materialized_summaries (provider_addon, name, key, value, updated_at)`
4. Sidebar widoki pytają najpierw cache → jeśli starsze niż 60s → revalidate w tle

Cache uwzględnia permissions: shell filtruje wyniki summary przez `permissions.can` przed pokazaniem.

### Event Publishing

Eventy są **publicznym kontraktem** addona. Inne addony subskrybują przez `needs.events`. Format:

```json
{
  "event": "crm.deal.stage_changed",
  "timestamp": "2026-05-19T10:30:00Z",
  "actor": {"user_id": "...", "addon_instance": "crm/main"},
  "resource": {"type": "deal", "id": "..."},
  "diff": {
    "stage": {"from": "offer", "to": "commit"},
    "commit": {"from": false, "to": true}
  },
  "context": {"deal_summary": {...}}  # snapshot dla subscriberów którym wystarczy
}
```

Eventy idą przez **TentaFlow MessageBody binary protocol** (już istnieje). Subskrypcja w manifeście.

### Action Provider

Akcje są wywoływane:
- Z UI (klik przycisku w view contribution)
- Z AI Broker (jak narzędzie LLM)
- Programowo z innego addona

**Definicja akcji w manifeście:**

```toml
[[provides.actions]]
name = "crm.create_deal"
input_schema = "schemas/create_deal.json"
output_schema = "schemas/deal.json"
risk = "mutating"
required_grants = ["crm.deal.write", "contacts.read_basic"]
confirmation = "required"
description = "Tworzy nowego deala. Wymaga contact_id, value_pln, stage."
```

`confirmation = "required"` → host przed wywołaniem pokazuje confirm dialog z diffem (input + computed effects). User klika OK → wywołanie idzie. Bez "required" akcja idzie natychmiast (np. read tools).

## UI surfaces (po stronie core platformy)

### M12b-style addon Settings → Dostęp (już ma mockup w tentavision-v1)

Reverse view: per addon — co addon używa (`needs`) i co wystawia (`provides`). Linki do P2 dla edycji grants.

### P2 Permissions matrix (już istnieje)

Pokazuje per-instance grants (sekcja inter-addon — patrz [02](./02-platform-permissions.md)).

## Provided host fn (dostępne dla każdego addona)

**Resources / Registry:**
- `registry.list_providers(resource_type) → list<addon_instance>`
- `registry.list_consumers(addon_instance) → list<dependency>`
- `registry.describe(addon_instance) → ManifestSummary`

**Views (rendering sidebars):**
- `views.list_contributions(slot, context) → list<PanelContribution>` — lista panel-id pasujących do slota
- `views.render(panel_id, context) → ViewModel` — woła addon-providera i zwraca view model (z cache jeśli summary istnieje)

**Queries (read):**
- `query.execute(addon_instance, query_name, input) → output` — pośrednik z auto-grant check

**Actions:**
- `action.invoke(addon_instance, action_name, input, ctx: {confirmed: bool}) → output | ConfirmRequest`
  - Jeśli `confirmation=required` i `ctx.confirmed=false` → zwraca `ConfirmRequest` z input + computed diff
  - User akceptuje → ponowne wywołanie z `confirmed=true`

**Events:**
- `events.publish(event_name, payload)` — addon publikuje
- `events.subscribe(event_name, handler_addon)` — host routuje (deklaracja w manifeście)

**Summaries:**
- `summary.get(addon_instance, summary_name, key) → value | null` (z cache)
- `summary.invalidate(addon_instance, summary_name, key)` — wymusz refresh

## Permissions

Każdy host fn check'uje grants z [02](./02-platform-permissions.md):
- `query.execute(crm/main, ...)` → wymaga że caller ma grant `crm.read` lub odpowiedni `read_*`
- `action.invoke(crm/main, "crm.create_deal", ...)` → wymaga grants z `required_grants` w manifeście
- `views.render` → filtruje wynik przez `permissions.can` na zasobach w view model

## Implementation order

1. **Manifest spec** — JSON schema dla `manifest.toml`. Walidator.
2. **Registry** — load manifestów ze wszystkich zainstalowanych addonów, INSERT do `addon_registry`. Host fn `registry.*`.
3. **Resource descriptors** — host fn `resource.describe(type, id)` zwracający canonical link + owner addon.
4. **Query routing** — `query.execute` → call do addon-providera przez WASM host fn.
5. **Event bus** — implementacja `events.publish/subscribe` na bazie istniejącego MessageBody.
6. **View slots** — definicja standardowych slotów. UI shell wywołuje `views.list_contributions` w odpowiednich miejscach.
7. **Materialized summaries** — schema + workers refreshujący on-event.
8. **Action invoke + confirm flow** — host fn + UI confirm dialog (jeden generyczny komponent `tf-confirm-dialog`).
9. **Per-instance grants** — `permission_rule_overrides` (z [02](./02-platform-permissions.md)) + UI w P2.
10. **Telemetry** — czas każdego query/action, ile failuje, ile confirm canceled.

## Otwarte decyzje

1. **Wersjonowanie kontraktów** — gdy crm/main v0.2 zmieni schema `crm.create_deal`, addony konsumujące (np. ai_broker) muszą się dowiedzieć. Rekomendacja: **manifest deklaruje `version` per query/action, konsumenci deklarują `min_version` w `needs`. Host odmawia połączenia jeśli mismatch**.

2. **Synchronous vs async actions** — `action.invoke` może długo trwać (np. promocja do realizacji = klon dużej liczby kosztów). Rekomendacja: **akcje &gt;1s muszą zwracać `JobId` zamiast wyniku, host subscribuje na event `action.completed`**. Synchroniczne dla &lt;1s.

3. **Resource ownership transfer** — czy można przekazać `deal` z `crm/main` do innej instancji `crm/test`? Rekomendacja: **nie na MVP**, owner addon = stały. Multi-instance jest gdy dwie firmy w jednym TentaFlow.

4. **Cross-addon transactions** — jeśli akcja A w crm woła akcję B w activity (np. create_deal → create_task), co jeśli B fail? Rekomendacja: **brak distributed transactions, B jest after-effect (event), eventual consistency**. UI pokazuje stan „częściowo wykonane" + retry.

5. **Cache invalidation correctness** — summary może być nieaktualne między eventem a refreshem. Rekomendacja: **w sidebar pokazuj cache + spinner-pasek u góry jeśli `updated_at < event.timestamp`**. User widzi że ładuje się świeże.
