# Plan implementacji — TentaFlow CRM + powiązane addony

Master index do wszystkich planów. Kolejność implementacji od dołu (fundament platformy) ku górze (addony).

**Zasada główna:** każdy plik opisuje co dokładnie ma robić każdy element systemu — wszystkie akcje użytkownika, wszystkie tooly AI, wszystkie kontrakty między addonami. Bez kodu — funkcjonalnie. Plik nie jest gotowy dopóki ktoś czytający nie wie co ma zaimplementować i w jakiej kolejności.

## Zawartość

### Platforma TentaFlow (fundament — musi powstać pierwsze)

| # | Plik | Po co |
|---|---|---|
| 00 | [`00-platform-roles-catalog.md`](./00-platform-roles-catalog.md) | Katalog ról administrowalny przez admina. Bez tego nie ma jak opisać kto jest kim w org-structure i w deal'ach. |
| 01 | [`01-platform-org-structure.md`](./01-platform-org-structure.md) | Edytor struktury organizacyjnej. Drzewo stanowisk, hierarchia raportowania, przypisanie osoba ↔ stanowisko ↔ rola. |
| 02 | [`02-platform-permissions.md`](./02-platform-permissions.md) | Model uprawnień: user + group + role + org-position → effective access. Per-instance grants między addonami. |
| 03 | [`03-platform-resource-contracts.md`](./03-platform-resource-contracts.md) | Wzorzec Resource Provider + View Contributions + Action Provider. Manifest addonu, host registry, materialized cache. |
| 04 | [`04-platform-ai-tool-broker.md`](./04-platform-ai-tool-broker.md) | Globalna pula narzędzi AI. Confirm dialog z diffem dla mutacji. Audit per wywołanie. |

### Addony (kolejność = priorytet adopcji)

| # | Plik | Po co | Zależy od |
|---|---|---|---|
| 10 | [`10-addon-contacts.md`](./10-addon-contacts.md) | Centralna baza kontaktów. Bez kontaktów nic dalej. | 00, 01, 02, 03 |
| 11 | [`11-addon-activity.md`](./11-addon-activity.md) | Wspólna warstwa zdarzeń (tasks, reminders, timeline events). Karmi Daily Inbox CRMa. | 03 |
| 12 | [`12-addon-documents.md`](./12-addon-documents.md) | Pliki + metadane + renderer PDF (karta akceptacji). | 03 |
| 13 | [`13-addon-billing.md`](./13-addon-billing.md) | Faktury sprzedaży + e-dokumenty kosztowe + refakturowanie. | 03, 10 |
| 14 | [`14-addon-calendar.md`](./14-addon-calendar.md) | Connector do Outlook/Google + meeting briefs. | 03, 10 |
| 15 | [`15-addon-crm.md`](./15-addon-crm.md) | **Najgrubszy plan.** Deal lifecycle (Lead→Won), Daily Sales Inbox, Pipeline, Forecast, kart akceptacji, refakturowanie. | wszystkie powyższe |

### Materiały kontekstowe

- [`../LIFECYCLE.md`](../LIFECYCLE.md) — reverse-engineering modułu Projekty z IntrApp
- [`../PLAN.md`](../PLAN.md) — pierwotny plan strategiczny (filozofia produktu, dekompozycja, mapowanie mockupów)
- Mockupy: `~/.gstack/projects/Slyb00ts-TentaFlow/designs/crm-v1/`

## Kolejność implementacji (fazy)

### Faza 0 — Platforma rdzeniowa (kilka tygodni)

Bez tego nic dalej nie zadziała.

1. **Roles catalog** ([00](./00-platform-roles-catalog.md)) — CRUD na rolach, kind, flagi, scope.
2. **Org structure** ([01](./01-platform-org-structure.md)) — drzewo stanowisk + przypisania osób.
3. **Permissions engine** ([02](./02-platform-permissions.md)) — kalkulator effective access (user + group + org).
4. **Resource contracts** ([03](./03-platform-resource-contracts.md)) — registry providerów, view contributions, materialized cache.
5. **AI Tool Broker** ([04](./04-platform-ai-tool-broker.md)) — globalna pula tooli, confirm dialog, audit.

### Faza 1 — Contacts + Activity (fundament addonowy)

6. **Contacts addon** ([10](./10-addon-contacts.md)) — firmy, osoby, relacje, grupy kapitałowe. Wystawia: ResourceDescriptor contact/company + RelationProvider + search.
7. **Activity addon** ([11](./11-addon-activity.md)) — tasks, reminders, timeline events. Wystawia: PanelContribution dla wszystkich addonów.

### Faza 2 — CRM minimalny (test adopcji)

8. **CRM minimalny** ([15](./15-addon-crm.md) — sekcje 1-4) — deal entity + stages + timeline + sidebar contribution na contact. Test: czy handlowiec używa.
9. **Daily Sales Inbox + Command palette ⌘K** — pierwszy ekran handlowca. M1 i M13 z mockupów.

### Faza 3 — AI Tools w pracy

10. **AI tools `crm.create_deal`, `crm.update_deal_from_context`, `email.draft_followup`** + chat sidebar (z [04](./04-platform-ai-tool-broker.md)).

### Faza 4 — Reszta addonów (rozszerzenia)

11. **Documents addon** ([12](./12-addon-documents.md)) + PDF renderer kart akceptacji.
12. **Billing addon** ([13](./13-addon-billing.md)) — koszty, refakturowanie, faktury sprzedaży.
13. **Calendar connector** ([14](./14-addon-calendar.md)) — Outlook/Google sync + meeting brief AI.

### Faza 5 — Dashboardy i analityka

14. **Dashboardy hierarchiczne** — handlowiec / dyrektor / zarząd na jednym silniku widgetów (z [15](./15-addon-crm.md) sekcje 8-10).
15. **Forecast workbench + snapshoty** (z [15](./15-addon-crm.md) sekcja 11).

### Faza 6 — Smart workflow

16. **Smart inbox + AI hygiene** + extraction z maili/spotkań ([15](./15-addon-crm.md) sekcja 12).

## Co nie jest w scope tego planu

- Timesheets jako osobny moduł (osoby na projekcie idą przez `responsible_persons`, nie przez timesheets)
- Klient KSeF / e-Faktur (osobny addon `ksef` w przyszłości — tylko punkty styku w Billing)
- Native e-mail client (Email pozostaje connector + threading)
- Subprojekty Presales / Realizacja / Utrzymanie (zastępujemy fazami stage'a deala)

## Zasady pisania planów

Każdy plik addona ma trzymać tę samą strukturę (dla porównywalności):

1. **Cel addona** (1 paragraf) — co i komu daje, czego nie robi.
2. **Domeny i encje** — pełna lista tabel z polami.
3. **Lifecycle / state machines** — diagramy stanów dla każdej kluczowej encji.
4. **UI surfaces** — lista ekranów (z linkiem do mockupu) + dla każdego: jakie akcje, co każda akcja wyzwala, jakie tooly woła.
5. **Provided contracts** — co addon wystawia innym przez manifest (resources, queries, actions, views, events, ai_tools).
6. **Consumed contracts** — czego potrzebuje od innych addonów (z konkretnymi grants).
7. **AI tools** — pełna lista wystawionych tooli ze schemami input/output, polityką confirm/audit.
8. **Permissions** — kto co widzi / edytuje (z odwołaniem do [02](./02-platform-permissions.md)).
9. **Migracja z IntrApp** — co stamtąd zachowujemy, mapowanie pól.
10. **Implementation order** — kroki implementacji w obrębie addona.
11. **Otwarte decyzje** — co jeszcze trzeba ustalić.

Plik addona powinien być wystarczający żeby ktoś (zespół, agent AI) mógł go zaimplementować bez zaglądania nigdzie indziej poza odwołania do innych planów.
